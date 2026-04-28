use bytes::Bytes;
use ext_php_rs::embed::{Sapi as SapiTrait, ServerContext as ServerContextTrait, *};
use ext_php_rs::ffi::{
    ext_php_rs_sapi_globals, php_module_shutdown, php_module_startup, sapi_shutdown, sapi_startup,
};
use std::ffi::CString;
use tokio::sync::mpsc::Sender;

pub struct ServerContext {
    sender: Option<Sender<Bytes>>,
    worker_id: u32,
}

impl ServerContextTrait for ServerContext {
    fn init_request_info(&self, _info: &mut RequestInfo) {}
    fn read_post(&mut self, _buf: &mut [u8]) -> usize {
        0
    }
    fn read_cookies(&self) -> Option<&str> {
        None
    }
    fn finish_request(&mut self) -> bool {
        true
    }
    fn is_request_finished(&self) -> bool {
        true
    }
}

impl ServerContext {
    pub fn init(worker_id: u32) {
        let sg = unsafe { &mut *ext_php_rs_sapi_globals() };
        if !sg.server_context.is_null() {
            panic!("server context already set");
        }

        sg.server_context = Box::into_raw(Box::new(Self {
            sender: None,
            worker_id,
        }))
        .cast();
    }

    pub fn start_request(sender: Sender<Bytes>) {
        let sg = unsafe { &mut *ext_php_rs_sapi_globals() };

        if sg.server_context.is_null() {
            panic!("server context not set");
        }

        let sc = unsafe { &mut *(sg.server_context as *mut Self) };
        sc.sender = Some(sender);
    }

    pub fn finish() {
        let sg = unsafe { &mut *ext_php_rs_sapi_globals() };
        if sg.server_context.is_null() {
            return;
        }

        let sc = unsafe { &mut *(sg.server_context as *mut Self) };
        // drop sender to signal end of request
        sc.sender = None;
    }

    pub fn destroy() {
        let sg = unsafe { &mut *ext_php_rs_sapi_globals() };
        if sg.server_context.is_null() {
            panic!("server context not set");
        }

        let _ = unsafe { Box::from_raw(sg.server_context as *mut Self) };
    }
}

pub struct Sapi(*mut SapiModule);

unsafe impl Send for Sapi {}
unsafe impl Sync for Sapi {}

impl SapiTrait for Sapi {
    type Context = ServerContext;
    fn name() -> &'static str {
        "ferrumphp"
    }
    fn pretty_name() -> &'static str {
        "FerrumPHP"
    }
    fn ub_write(_ctx: &mut ServerContext, buf: &[u8]) -> usize {
        let body = format!(
            "{} from Worker #{}",
            String::from_utf8_lossy(buf).to_string(),
            _ctx.worker_id
        );

        _ctx.sender
            .as_ref()
            .expect("ub_write found no sender")
            .blocking_send(Bytes::from(body))
            .unwrap();
        buf.len()
    }
    fn log_message(msg: &str, _: i32) {
        eprintln!("{msg}");
    }
}

impl Sapi {
    pub fn new() -> Self {
        let module = Self::build_module().expect("Failed to build SAPI");
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
