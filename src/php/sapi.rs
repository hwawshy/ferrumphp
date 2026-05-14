use crate::php::context::ServerContext;
use crate::php::ffi::{ferrumphp_error, php_handle_aborted_connection, sapi_send_headers};
use bytes::{Buf, Bytes};
use ext_php_rs::builders::SapiBuilder;
use ext_php_rs::embed::{
    SapiModule, ServerVarRegistrar, ext_php_rs_sapi_shutdown, ext_php_rs_sapi_startup,
};
use ext_php_rs::ffi::{
    ZEND_RESULT_CODE_FAILURE, ZEND_RESULT_CODE_SUCCESS, php_module_shutdown, php_module_startup,
    sapi_headers_struct, sapi_shutdown, sapi_startup,
};
use ext_php_rs::types::Zval;
use ext_php_rs::zend::SapiGlobals;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{HeaderMap, Response, StatusCode};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::Read;
use std::str::FromStr;

#[repr(i32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum SapiHeaderSendResult {
    SendFailed = 3,
    //DoSend = 2,
    SentSuccessfully = 1,
}

impl From<SapiHeaderSendResult> for c_int {
    fn from(value: SapiHeaderSendResult) -> Self {
        value as c_int
    }
}

pub struct Sapi(*mut SapiModule);

unsafe impl Send for Sapi {}
unsafe impl Sync for Sapi {}

impl Sapi {
    pub fn new() -> Self {
        let mut module = SapiBuilder::new("ferrumphp", "FerrumPHP")
            .ub_write_function(ub_write)
            .flush_function(flush)
            .deactivate_function(deactivate)
            .log_message_function(log_message)
            .send_headers_function(send_headers)
            .read_post_function(read_post)
            .read_cookies_function(read_cookies)
            .register_server_variables_function(register_server_variables)
            .build()
            .expect("Failed to build SAPI");

        // C-variadic functions are unstable in Rust
        module.sapi_error = Some(ferrumphp_error);

        let sapi_ptr = module.into_raw();

        unsafe {
            ext_php_rs_sapi_startup();

            sapi_startup(sapi_ptr);

            php_module_startup(sapi_ptr, std::ptr::null_mut());
        }

        Self(sapi_ptr)
    }
}

impl Drop for Sapi {
    fn drop(&mut self) {
        unsafe {
            php_module_shutdown();

            sapi_shutdown();

            ext_php_rs_sapi_shutdown();

            let sapi = Box::from_raw(self.0);
            let _ = CString::from_raw(sapi.name);
            let _ = CString::from_raw(sapi.pretty_name);

            if !sapi.executable_location.is_null() {
                let _ = CString::from_raw(sapi.executable_location);
            }

            if !sapi.ini_entries.is_null() {
                let _ = CString::from_raw(sapi.ini_entries.cast_mut());
            }

            if !sapi.php_ini_path_override.is_null() {
                let _ = CString::from_raw(sapi.php_ini_path_override);
            }
        }
    }
}

extern "C" fn ub_write(str: *const c_char, str_length: usize) -> usize {
    println!("ub_write");

    if str.is_null() || str_length == 0 {
        return 0;
    }
    let Some(ctx) = ServerContext::get_mut() else {
        return 0;
    };

    let buf = unsafe { std::slice::from_raw_parts(str.cast::<u8>(), str_length) };

    // let body = format!(
    //     "{} from Worker #{}",
    //     String::from_utf8_lossy(buf).to_string(),
    //     ctx.worker_id
    // );

    // This will buffer if headers are not sent yet. Maybe some memory check here?
    let sender = ctx.response_tx.as_ref().expect("ub_write found no sender");

    if let Err(_) = sender.blocking_send(Bytes::from(buf)) {
        println!("aborted connection");
        unsafe {
            php_handle_aborted_connection();
        }

        return 0;
    }

    buf.len()
}

extern "C" fn log_message(message: *const c_char, _syslog_type: c_int) {
    if message.is_null() {
        return;
    }
    let msg = unsafe { CStr::from_ptr(message) };
    let msg_str = msg.to_string_lossy();
    eprintln!("{msg_str}");
}

extern "C" fn flush(_server_context: *mut c_void) {
    // Force sending of headers, which will also flush any buffered body bytes
    println!("flush");

    if let Some(_) = ServerContext::get_mut() {
        // Our send_headers fails on client disconnection
        if unsafe { sapi_send_headers() } == ZEND_RESULT_CODE_FAILURE {
            unsafe { php_handle_aborted_connection() }
        }
    }
}

extern "C" fn send_headers(sapi_headers: *mut sapi_headers_struct) -> c_int {
    println!("send_headers");

    if SapiGlobals::get_mut().request_info().no_headers {
        return SapiHeaderSendResult::SentSuccessfully.into();
    }

    let Some(ctx) = ServerContext::get_mut() else {
        // @todo when can this happen? panic here?
        return SapiHeaderSendResult::SendFailed.into();
    };

    // send_headers may be called multiple times upon failure
    let Some(sender) = ctx.head_tx.take() else {
        return SapiHeaderSendResult::SendFailed.into();
    };

    let mut map = HeaderMap::new();
    let mut status: Option<StatusCode> = None;

    if !sapi_headers.is_null() {
        let sapi_headers = unsafe { &mut *sapi_headers };

        if !sapi_headers.http_status_line.is_null() {
            let line = unsafe { CStr::from_ptr(sapi_headers.http_status_line) }.to_bytes();

            status = line.iter().position(|&b| b == b' ').and_then(|i| {
                let rest = &line[i + 1..];
                rest.iter()
                    .position(|&b| b == b' ')
                    .and_then(|j| StatusCode::from_bytes(&rest[..j]).ok())
            });
        }

        if status.is_none() {
            status = StatusCode::from_u16(sapi_headers.http_response_code as u16).ok()
        }

        for header in sapi_headers.headers() {
            let Some(value) = header.value() else {
                continue;
            };

            if let Ok(name) = HeaderName::from_str(header.name())
                && let Ok(value) = HeaderValue::from_str(value)
            {
                map.append(name, value);
            }
        }
    }

    let (mut parts, _) = Response::new(()).into_parts();

    parts.headers = map;
    parts.status = status.unwrap_or(StatusCode::OK);

    if let Err(_) = sender.send(parts) {
        // Future dropped together with receiver. Happens when client disconnects
        return SapiHeaderSendResult::SendFailed.into();
    }

    SapiHeaderSendResult::SentSuccessfully.into()
}

extern "C" fn read_post(buffer: *mut c_char, length: usize) -> usize {
    println!("read_post");

    if buffer.is_null() || length == 0 {
        return 0;
    }

    let Some(ctx) = ServerContext::get_mut() else {
        // @todo when can this happen? panic here?
        return 0;
    };

    let Some(ref mut body_rx) = ctx.request_body_rx else {
        // @todo when can this happen? panic here?
        return 0;
    };

    let buf = unsafe { std::slice::from_raw_parts_mut(buffer.cast::<u8>(), length) };

    let mut written = 0;

    // one PHP read may consume multiple chunks
    // one chunk may span multiple PHP reads
    while written < buf.len() {
        if ctx.current_request_body_chunk.is_none() {
            ctx.current_request_body_chunk = body_rx.blocking_recv();
        }

        let Some(ref mut chunk) = ctx.current_request_body_chunk else {
            break;
        };

        let to_read = chunk.len().min(buf.len() - written);

        written += match chunk.reader().read(&mut buf[written..written + to_read]) {
            Err(_) => break,
            Ok(n) => n,
        };

        if chunk.is_empty() {
            ctx.current_request_body_chunk = None;
        }
    }

    written
}

extern "C" fn read_cookies() -> *mut c_char {
    let Some(ctx) = ServerContext::get_mut() else {
        return std::ptr::null_mut();
    };

    match ctx.cookies {
        Some(ref cookies) => cookies.as_ptr().cast_mut(),
        None => std::ptr::null_mut(),
    }
}

extern "C" fn register_server_variables(vars: *mut Zval) {
    if vars.is_null() {
        return;
    }

    let globals = SapiGlobals::get_mut();
    let request_info = globals.request_info();

    let mut registrar = unsafe { ServerVarRegistrar::from_raw(vars) };

    registrar.register("SERVER_SOFTWARE", "ferrumphp/0.1");

    if let Some(request_uri) = request_info.request_uri() {
        registrar.register("REQUEST_URI", request_uri);
    }

    if let Some(request_method) = request_info.request_method() {
        registrar.register("REQUEST_METHOD", request_method);
    }

    if let Some(query_string) = request_info.query_string() {
        registrar.register("QUERY_STRING", query_string);
    }
}

extern "C" fn deactivate() -> c_int {
    println!("deactivate");
    if let Some(ctx) = ServerContext::get_mut() {
        ctx.finish_request();
    }

    ZEND_RESULT_CODE_SUCCESS
}
