use bytes::Bytes;
use ext_php_rs::embed::{ext_php_rs_sapi_per_thread_init, ext_php_rs_sapi_per_thread_shutdown};
use ext_php_rs::ffi::{
    ZEND_RESULT_CODE_SUCCESS, ext_php_rs_sapi_globals, php_execute_script, php_request_shutdown,
    php_request_startup, zend_destroy_file_handle, zend_file_handle, zend_stream_init_filename,
};
use ext_php_rs::zend::SapiGlobals;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::http::request::Parts;
use hyper::{HeaderMap, Request, StatusCode, Version};
use std::ffi::{CString, NulError, c_int};
use std::mem::MaybeUninit;
use std::path::PathBuf;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot::Sender as OneshotSender;

pub struct ServerContext {
    pub worker_id: u32,
    pub response_tx: Option<Sender<Bytes>>,
    pub headers_tx: Option<OneshotSender<HeaderMap>>,
    pub request_body_rx: Option<Receiver<Bytes>>,
    pub current_request_body_chunk: Option<Bytes>,
    pub cookies: Option<CString>,
    _private: (),
}

impl ServerContext {
    fn new(worker_id: u32) -> Self {
        Self {
            worker_id,
            response_tx: None,
            headers_tx: None,
            request_body_rx: None,
            current_request_body_chunk: None,
            cookies: None,
            _private: (),
        }
    }

    pub fn finish_request(&mut self) {
        self.response_tx = None;
        self.request_body_rx = None;
        self.headers_tx = None;
        self.cookies = None;
        self.current_request_body_chunk = None;
    }

    pub fn get_mut() -> Option<&'static mut Self> {
        let globals = unsafe { ext_php_rs_sapi_globals().as_mut() }.expect("Invalid SAPI globals");

        unsafe { globals.server_context.cast::<Self>().as_mut() }
    }
}

pub struct WorkerContext {
    server_ctx: Box<ServerContext>,
}

impl WorkerContext {
    pub fn new(worker_id: u32) -> Self {
        unsafe { ext_php_rs_sapi_per_thread_init() }

        let mut sg = SapiGlobals::get_mut();

        if !sg.server_context.is_null() {
            panic!("server context already set");
        }

        let mut server_ctx = Box::new(ServerContext::new(worker_id));

        sg.server_context = server_ctx.as_mut() as *mut _ as *mut _;

        Self { server_ctx }
    }

    pub fn handle_request(
        &mut self,
        request_head: Parts,
        request_body_rx: Receiver<Bytes>,
        header_tx: OneshotSender<HeaderMap>,
        response_tx: Sender<Bytes>,
    ) {
        self.server_ctx.response_tx = Some(response_tx);
        self.server_ctx.headers_tx = Some(header_tx);

        self.server_ctx.request_body_rx = Some(request_body_rx);

        self.server_ctx.cookies = request_head
            .headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| CString::new(s).ok());

        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("src/php/test.php");

        let filename = path.to_str().unwrap();

        let request_context = PhpRequestContext::new(request_head, filename).unwrap();

        request_context.execute();
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

struct PhpRequestContext {
    filename: CString,
    proto_num: i32,
    method: CString,
    uri: CString,
    query: Option<CString>,
    content_type: Option<CString>,
    content_length: Option<i64>,
}

impl PhpRequestContext {
    // @TODO validation and suitable error type
    pub fn new(head: Parts, filename: &str) -> Result<Self, NulError> {
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
        })
    }

    pub fn execute(&self) {
        unsafe {
            self.populate_request_info();

            if php_request_startup() != ZEND_RESULT_CODE_SUCCESS {
                panic!("request startup failed");
            }

            let mut file_handle = MaybeUninit::<zend_file_handle>::uninit();

            zend_stream_init_filename(file_handle.as_mut_ptr(), self.filename.as_ptr());

            let mut file_handle = file_handle.assume_init();
            file_handle.primary_script = true;

            if !php_execute_script(&mut file_handle) {
                panic!("error executing script");
            }

            zend_destroy_file_handle(&mut file_handle);

            php_request_shutdown(std::ptr::null_mut());
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

        sapi_globals.request_info.query_string = std::ptr::null_mut();
        sapi_globals.request_info.content_length = 0;
        sapi_globals.request_info.content_type = std::ptr::null();
    }
}
