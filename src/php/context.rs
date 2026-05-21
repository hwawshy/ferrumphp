use bytes::Bytes;
use ext_php_rs::embed::{ext_php_rs_sapi_per_thread_init, ext_php_rs_sapi_per_thread_shutdown};
use ext_php_rs::ffi::{
    ZEND_RESULT_CODE_SUCCESS, ext_php_rs_sapi_globals, php_execute_script, php_request_shutdown,
    php_request_startup, zend_destroy_file_handle, zend_file_handle, zend_stream_init_filename,
};
use ext_php_rs::zend::{SapiGlobals, try_catch, try_catch_first};
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::http::request::Parts as RequestParts;
use hyper::http::response::Parts as ResponseParts;
use hyper::{StatusCode, Version};
use std::ffi::{CString, NulError, c_int};
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
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
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("src/php/test.php");

        let filename = path.to_str().unwrap();

        let request_ctx = PhpRequestContext::new(
            request_head,
            filename,
            request_body_rx,
            head_tx,
            response_tx
        ).unwrap();

        self.request_ctx = Some(request_ctx);

        if let Err(_) = unsafe { self.request_ctx.as_ref().unwrap().execute() } {
            self.finish_request();

            return Err(());
        }

        Ok(())
    }

    pub fn get_mut() -> Option<&'static mut Self> {
        let globals = unsafe { ext_php_rs_sapi_globals().as_mut() }.expect("Invalid SAPI globals");

        unsafe { globals.server_context.cast::<Self>().as_mut() }
    }

    pub fn get_request_context_mut() -> Option<&'static mut PhpRequestContext> {
        Self::get_mut().and_then(|t| Option::from(&mut t.request_ctx))
    }
}

struct WorkerConfig {
    worker_id: usize,
    //bind: SocketAddr,
    //filename: PathBuf
}

pub struct WorkerContext {
    server_ctx: Box<ServerContext>,
}

impl WorkerContext {
    pub fn new(worker_id: usize) -> Self {
        unsafe { ext_php_rs_sapi_per_thread_init() }

        let mut sg = SapiGlobals::get_mut();

        if !sg.server_context.is_null() {
            panic!("server context already set");
        }

        let mut server_ctx = Box::new(ServerContext::new(WorkerConfig {worker_id}));

        sg.server_context = server_ctx.as_mut() as *mut _ as *mut _;

        Self { server_ctx }
    }

    pub fn handle_request(
        &mut self,
        request_head: RequestParts,
        request_body_rx: Option<Receiver<Bytes>>,
        head_tx: OneshotSender<ResponseParts>,
        response_tx: Sender<Bytes>,
    ) -> Result<(), ()> {
        self.server_ctx.handle_request(request_head, request_body_rx, head_tx, response_tx)
    }
}

impl Drop for WorkerContext {
    fn drop(&mut self) {
        SapiGlobals::get_mut().server_context = std::ptr::null_mut();

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
    //php_headers: Vec<(CString, Vec<u8>)>
}

impl PhpRequestContext {
    // @TODO validation and suitable error type
    pub fn new(
        head: RequestParts,
        filename: &str,
        request_body_rx: Option<Receiver<Bytes>>,
        head_tx: OneshotSender<ResponseParts>,
        response_tx: Sender<Bytes>,
    ) -> Result<Self, NulError> {
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

        // let mut php_headers: Vec<(CString, Vec<u8>)> = vec![];
        //
        // // @todo improve performance, look into php interned strings for header keys
        // for (name, value) in &headers {
        //     let mut key = String::from("HTTP_");
        //
        //     key.push_str(
        //         &name.as_str()
        //             .to_ascii_uppercase()
        //             .replace('-', "_")
        //     );
        //
        //     php_headers.push((CString::new(key).unwrap(), value.as_bytes().to_vec()));
        // }

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
            if php_request_startup() != ZEND_RESULT_CODE_SUCCESS {
                return false;
            }

            php_execute_script(&raw mut file_handle);

            // PHP expects this to be called before request shutdown
            zend_destroy_file_handle(&raw mut file_handle);

            attempted_shutdown = true;

            php_request_shutdown(std::ptr::null_mut());

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

    unsafe fn populate_request_info(&self) {
        let mut sapi_globals = SapiGlobals::get_mut();

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
    }
}

impl Drop for PhpRequestContext {
    fn drop(&mut self) {
        let mut sapi_globals = SapiGlobals::get_mut();

        sapi_globals.request_info.request_method = std::ptr::null();
        sapi_globals.request_info.request_uri = std::ptr::null_mut();
        sapi_globals.request_info.cookie_data = std::ptr::null_mut();

        sapi_globals.request_info.query_string = std::ptr::null_mut();
        sapi_globals.request_info.content_length = 0;
        sapi_globals.request_info.content_type = std::ptr::null();
    }
}
