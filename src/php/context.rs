use bytes::Bytes;
use ext_php_rs::embed::{ext_php_rs_sapi_per_thread_init, ext_php_rs_sapi_per_thread_shutdown};
use ext_php_rs::ffi::{
    ZEND_RESULT_CODE_SUCCESS, ext_php_rs_sapi_globals, php_execute_script, php_request_shutdown,
    php_request_startup, zend_destroy_file_handle, zend_file_handle, zend_stream_init_filename,
};
use ext_php_rs::zend::SapiGlobals;
use hyper::body::Incoming;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE};
use hyper::{HeaderMap, Request, StatusCode, Version};
use std::ffi::{CString, NulError, c_int};
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot::Sender as OneshotSender;

pub struct ServerContext {
    pub worker_id: u32,
    local_addr: Option<SocketAddr>,
    peer_addr: Option<SocketAddr>,
    pub response_tx: Option<Sender<Bytes>>,
    pub headers_tx: Option<OneshotSender<HeaderMap>>,
}

impl ServerContext {
    fn new(worker_id: u32) -> Self {
        Self {
            worker_id,
            local_addr: None,
            peer_addr: None,
            response_tx: None,
            headers_tx: None,
        }
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
        request: Request<Incoming>,
        response_tx: Sender<Bytes>,
        header_tx: OneshotSender<HeaderMap>,
    ) {
        self.server_ctx.response_tx = Some(response_tx);
        self.server_ctx.headers_tx = Some(header_tx);

        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("src/php/test.php");

        let filename = path.to_str().unwrap();

        let request_context = PhpRequestContext::new(request, filename).unwrap();

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
    pub fn new(request: Request<Incoming>, filename: &str) -> Result<Self, NulError> {
        let filename = CString::new(filename)?;

        let method = CString::new(request.method().as_str())?;

        let uri = request.uri();

        let query = uri.query().and_then(|query| CString::new(query).ok());

        let uri = CString::new(uri.to_string())?;

        let headers = request.headers();

        let content_length = headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<i64>().ok());

        let content_type = headers
            .get(CONTENT_TYPE)
            .and_then(|c| c.to_str().ok())
            .and_then(|c| CString::new(c).ok());

        let proto_num = match request.version() {
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
