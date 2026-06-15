use crate::CONFIG;
use crate::cli::Config;
use crate::php::ffi::php_handle_auth_data;
use crate::php::interned::{INTERNED, Interned};
use bytes::Bytes;
use ext_php_rs::boxed::ZBox;
use ext_php_rs::embed::{ext_php_rs_sapi_per_thread_init, ext_php_rs_sapi_per_thread_shutdown};
use ext_php_rs::ffi::{
    ZEND_RESULT_CODE_SUCCESS, ext_php_rs_sapi_globals, php_execute_script, php_request_shutdown,
    php_request_startup, zend_destroy_file_handle, zend_file_handle, zend_stream_init_filename,
};
use ext_php_rs::types::{ZendHashTable, ZendStr, Zval};
use ext_php_rs::zend::{SapiGlobals, try_catch, try_catch_first};
use hyper::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::http::Extensions;
use hyper::http::request::Parts as RequestParts;
use hyper::http::response::Parts as ResponseParts;
use hyper::{HeaderMap, StatusCode, Version};
use std::ffi::{CStr, CString, NulError, c_int};
use std::mem::MaybeUninit;
use std::net::{IpAddr, SocketAddr};
use std::panic::AssertUnwindSafe;
use std::time::Instant;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot::Sender as OneshotSender;

pub struct ServerContext {
    pub request_ctx: Option<PhpRequestContext>,
    worker_config: WorkerConfig,
    _private: (),
}

impl ServerContext {
    fn new(worker_config: WorkerConfig) -> Self {
        Self {
            request_ctx: None,
            worker_config,
            _private: (),
        }
    }

    pub fn finish_request(&mut self) {
        self.request_ctx = None;
    }

    fn handle_request(
        &mut self,
        request_head: RequestParts,
        request_body_rx: Option<Receiver<Bytes>>,
        head_tx: OneshotSender<ResponseParts>,
        response_tx: Sender<Bytes>,
    ) -> Result<(), ()> {
        let request_ctx =
            PhpRequestContext::new(request_head, request_body_rx, head_tx, response_tx).unwrap();

        self.request_ctx = Some(request_ctx);

        if let Err(_) = unsafe { self.request_ctx.as_ref().unwrap().execute() } {
            self.finish_request();

            return Err(());
        }

        Ok(())
    }

    pub fn get_mut() -> Option<&'static mut Self> {
        let globals = WorkerContext::get_sapi_globals_mut();

        unsafe { globals.server_context.cast::<Self>().as_mut() }
    }

    pub fn get_request_context_mut() -> Option<&'static mut PhpRequestContext> {
        Self::get_mut().and_then(|t| Option::from(&mut t.request_ctx))
    }
}

struct WorkerConfig {
    worker_id: usize,
}

pub struct WorkerContext {
    server_ctx: Box<ServerContext>,
}

impl WorkerContext {
    pub fn new(worker_id: usize) -> Self {
        unsafe { ext_php_rs_sapi_per_thread_init() }

        let sg = Self::get_sapi_globals_mut();

        if !sg.server_context.is_null() {
            panic!("server context already set");
        }

        let mut server_ctx = Box::new(ServerContext::new(WorkerConfig { worker_id }));

        sg.server_context = server_ctx.as_mut() as *mut _ as *mut _;

        Self { server_ctx }
    }

    pub fn get_sapi_globals() -> &'static SapiGlobals {
        unsafe { &*ext_php_rs_sapi_globals() }
    }

    pub fn get_sapi_globals_mut() -> &'static mut SapiGlobals {
        unsafe { &mut *ext_php_rs_sapi_globals() }
    }

    pub fn handle_request(
        &mut self,
        request_head: RequestParts,
        request_body_rx: Option<Receiver<Bytes>>,
        head_tx: OneshotSender<ResponseParts>,
        response_tx: Sender<Bytes>,
    ) -> Result<(), ()> {
        self.server_ctx
            .handle_request(request_head, request_body_rx, head_tx, response_tx)
    }
}

impl Drop for WorkerContext {
    fn drop(&mut self) {
        Self::get_sapi_globals_mut().server_context = std::ptr::null_mut();

        unsafe {
            ext_php_rs_sapi_per_thread_shutdown();
        }
    }
}

pub struct PhpRequestContext {
    pub filename: CString,
    pub proto_num: i32,
    pub method: CString,
    pub uri: CString,
    pub query: Option<CString>,
    pub content_type: Option<CString>,
    pub content_length: Option<i64>,
    pub cookies: Option<CString>,
    pub request_body_rx: Option<Receiver<Bytes>>,
    pub current_request_body_chunk: Option<Bytes>,
    pub head_tx: Option<OneshotSender<ResponseParts>>,
    pub response_tx: Sender<Bytes>,
    pub headers: HeaderMap,
    extensions: Extensions,
}

impl PhpRequestContext {
    // @TODO validation and suitable error type
    pub fn new(
        head: RequestParts,
        request_body_rx: Option<Receiver<Bytes>>,
        head_tx: OneshotSender<ResponseParts>,
        response_tx: Sender<Bytes>,
    ) -> Result<Self, NulError> {
        let filename = CONFIG.get().unwrap().entrypoint.to_str().unwrap();

        let filename = CString::new(filename)?;

        let method = CString::new(head.method.as_str())?;

        let uri = head.uri;

        let query = uri.query().and_then(|query| CString::new(query).ok());

        let uri = CString::new(uri.to_string())?;

        let headers = head.headers;

        let content_length = headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok());

        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|c| c.to_str().ok())
            .and_then(|c| CString::new(c).ok());

        let cookies = headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| CString::new(s).ok());

        let proto_num = match head.version {
            Version::HTTP_09 => 900,
            Version::HTTP_10 => 1000,
            Version::HTTP_11 => 1100,
            Version::HTTP_2 => 2000,
            Version::HTTP_3 => 3000,
            _ => unreachable!(),
        };

        Ok(Self {
            filename,
            proto_num,
            method,
            uri,
            query,
            content_type,
            content_length,
            cookies,
            request_body_rx,
            current_request_body_chunk: None,
            head_tx: Some(head_tx),
            response_tx,
            headers,
            extensions: head.extensions,
        })
    }

    unsafe fn execute(&self) -> Result<(), ()> {
        unsafe { self.populate_request_info() };

        let mut attempted_shutdown = false;

        let mut file_handle = MaybeUninit::<zend_file_handle>::uninit();

        unsafe {
            zend_stream_init_filename(file_handle.as_mut_ptr(), self.filename.as_ptr());
        };

        let mut file_handle = unsafe { file_handle.assume_init() };
        file_handle.primary_script = true;

        let result = try_catch_first(AssertUnwindSafe(|| unsafe {
            let result = self.in_span("php_request_startup", || { php_request_startup() });

            if result != ZEND_RESULT_CODE_SUCCESS {
                return false;
            }

            self.in_span("php_execute_script", || { php_execute_script(&raw mut file_handle); });

            // PHP expects this to be called before request shutdown
            zend_destroy_file_handle(&raw mut file_handle);

            attempted_shutdown = true;

            self.in_span("php_request_shutdown", || { php_request_shutdown(std::ptr::null_mut()); });

            true
        }));

        match result {
            Err(_) => {
                // bailout
                if !attempted_shutdown {
                    // @todo look into freeing last_error_message if shutdown bails out
                    let _ = try_catch(AssertUnwindSafe(|| unsafe {
                        zend_destroy_file_handle(&raw mut file_handle);

                        php_request_shutdown(std::ptr::null_mut());
                    }));
                }

                Err(())
            }
            Ok(false) => {
                // request startup failed
                unsafe { zend_destroy_file_handle(&raw mut file_handle) };

                Err(())
            }
            Ok(true) => Ok(()),
        }
    }

    fn in_span<T>(
        &self,
        phase: &'static str,
        f: impl FnOnce() -> T,
    ) -> T {
        let span = tracing::info_span!(
        "php_execution_phase",
        phase = phase,
        duration = tracing::field::Empty,
    );

        let start = Instant::now();

        let result = {
            let _guard = span.enter();
            f()
        };

        span.record(
            "duration",
            start.elapsed().as_micros(),
        );

        result
    }

    unsafe fn populate_request_info(&self) {
        let sapi_globals = WorkerContext::get_sapi_globals_mut();

        sapi_globals.sapi_headers.http_response_code = StatusCode::OK.as_u16() as c_int;

        sapi_globals.request_info.request_method = self.method.as_ptr();
        sapi_globals.request_info.request_uri = self.uri.as_ptr() as *mut _;

        sapi_globals.request_info.query_string = self
            .query
            .as_ref()
            .map(|c| c.as_ptr() as *mut _)
            .unwrap_or_default();
        sapi_globals.request_info.content_length = self.content_length.unwrap_or(0);
        sapi_globals.request_info.content_type = self
            .content_type
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or_default();
        sapi_globals.request_info.proto_num = self.proto_num;

        // PHP expects the value of auth header to be a C string and copies anything it wants,
        // so we are free to drop the C string after the php_handle_auth_data function call.
        if let Some(auth) = self.headers.get(AUTHORIZATION) {
            if let Ok(auth) = CString::new(auth.as_bytes()) {
                unsafe {
                    php_handle_auth_data(auth.as_ptr());
                }
            }
        }
    }

    pub unsafe fn register_server_variables(&self, vars: &mut ZendHashTable) {
        let config = CONFIG.get().expect("Config not initialized");
        let interned = INTERNED.get().expect("Interned strings not initialized");

        // hard-coded values
        let _ = vars.insert(
            &interned.server_software,
            self.zval_from_interned(&interned.ferrumphp),
        );
        let _ = vars.insert(
            &interned.gateway_interface,
            self.zval_from_interned(&interned.cgi11),
        );
        let _ = vars.insert(
            &interned.server_addr,
            self.zval_from_interned(&interned.server_addr_value),
        );
        let _ = vars.insert(
            &interned.server_port,
            self.zval_from_interned(&interned.server_port_value),
        );
        let _ = vars.insert(
            &interned.script_filename,
            self.zval_from_interned(&interned.script_filename_value),
        );
        let _ = vars.insert(
            &interned.document_root,
            self.zval_from_interned(&interned.document_root_value),
        );
        let _ = vars.insert(
            &interned.script_name,
            self.zval_from_interned(&interned.script_name_value),
        );

        self.register_path_variables(vars, interned);
        self.register_remote_variables(vars, interned, config);

        let _ = vars.insert(&interned.request_uri, self.zval_from_cstr(&self.uri));
        let _ = vars.insert(&interned.request_method, self.zval_from_cstr(&self.method));

        if let Some(ref query) = self.query {
            let _ = vars.insert(&interned.query_string, self.zval_from_cstr(query));
        }

        if let Some(ref content_type) = self.content_type {
            let _ = vars.insert(&interned.content_type, self.zval_from_cstr(content_type));
        }

        if let Some(content_length) = self.content_length {
            let mut zval = Zval::new();
            zval.set_long(content_length);

            let _ = vars.insert(&interned.content_length, zval);
        }

        // SERVER_NAME — from Host header
        if let Some(host) = self
            .headers
            .get(HOST)
            .and_then(|v| v.to_str().ok())
            .and_then(|host| host.split(':').next())
        {
            let _ = vars.insert(&interned.server_name, self.zval_from_str(host));
        }

        // SERVER_PROTOCOL — from request version
        let protocol = match self.proto_num {
            900 => "HTTP/0.9",
            1000 => "HTTP/1.0",
            1100 => "HTTP/1.1",
            2000 => "HTTP/2",
            3000 => "HTTP/3",
            _ => "HTTP/1.1",
        };

        let _ = vars.insert(&interned.server_protocol, self.zval_from_str(protocol));

        for (header_name, header_value) in self.headers.iter() {
            if header_name == CONTENT_TYPE || header_name == CONTENT_LENGTH {
                continue;
            }

            if header_name.as_str().contains('_') {
                continue;
            }

            let mut zval = Zval::new();
            zval.set_binary(header_value.as_ref().to_vec());

            match interned.map_header_name(header_name) {
                Some(interned_name) => {
                    let _ = vars.insert(interned_name, zval);
                }
                None => {
                    let mut key = String::from("HTTP_");

                    key.push_str(&header_name.as_str().to_ascii_uppercase().replace('-', "_"));

                    let _ = vars.insert(key, zval);
                }
            };
        }
    }

    /// PATH_INFO, PHP_SELF
    fn register_path_variables(&self, vars: &mut ZendHashTable, interned: &Interned) {
        let entrypoint = &CONFIG.get().unwrap().entrypoint;
        let document_root = entrypoint.parent().unwrap().to_str().unwrap();
        let script_name = entrypoint
            .to_str()
            .unwrap()
            .strip_prefix(document_root)
            .unwrap();

        let uri = self.uri.to_str().unwrap();

        let path_info = if let Some(rest) = uri.strip_prefix(script_name) {
            match rest {
                "" => None,
                _ => Some(rest),
            }
        } else {
            match uri {
                "" => None,
                _ => Some(uri),
            }
        };

        if let Some(path_info) = path_info {
            let _ = vars.insert(&interned.path_info, self.zval_from_str(path_info));

            let mut php_self = String::with_capacity(script_name.len() + path_info.len());

            php_self.push_str(script_name);
            php_self.push_str(path_info);

            let _ = vars.insert(&interned.php_self, self.zval_from_str(&php_self));
        }
    }

    fn register_remote_variables(
        &self,
        vars: &mut ZendHashTable,
        interned: &Interned,
        config: &Config,
    ) {
        let Some(remote) = self.extensions.get::<SocketAddr>() else {
            return;
        };

        if config.is_trusted_proxy(remote.ip())
            && let Some(ip) = self.forwarded_client_ip()
        {
            let _ = vars.insert(&interned.remote_addr, self.zval_from_str(&ip.to_string()));
        } else {
            let _ = vars.insert(
                &interned.remote_addr,
                self.zval_from_str(&remote.ip().to_string()),
            );
        }

        let mut zval = Zval::new();
        let _ = zval.set_long(remote.port());

        let _ = vars.insert(&interned.remote_port, zval);
    }

    pub fn forwarded_client_ip(&self) -> Option<IpAddr> {
        let value = self.headers.get("x-forwarded-for")?;
        let value = value.to_str().ok()?;

        let first = value.split(',').next()?.trim();

        first.parse().ok()
    }

    fn zval_from_str(&self, value: &str) -> Zval {
        let mut zval = Zval::new();
        let _ = zval.set_string(value, false);

        zval
    }

    fn zval_from_interned(&self, interned: &ZendStr) -> Zval {
        let mut zval = Zval::new();
        zval.set_zend_string(unsafe { ZBox::from_raw(interned.as_ptr().cast_mut()) });

        zval
    }

    fn zval_from_cstr(&self, cstr: &CStr) -> Zval {
        let zend_str = ZendStr::from_c_str(cstr, false);

        let mut zval = Zval::new();
        zval.set_zend_string(zend_str);

        zval
    }
}

impl Drop for PhpRequestContext {
    fn drop(&mut self) {
        let sapi_globals = WorkerContext::get_sapi_globals_mut();

        sapi_globals.request_info.request_method = std::ptr::null();
        sapi_globals.request_info.request_uri = std::ptr::null_mut();
        sapi_globals.request_info.cookie_data = std::ptr::null_mut();

        sapi_globals.request_info.query_string = std::ptr::null_mut();
        sapi_globals.request_info.content_length = 0;
        sapi_globals.request_info.content_type = std::ptr::null();
    }
}
