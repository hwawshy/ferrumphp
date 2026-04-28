use crate::php::Job;
use crate::php::sapi::ServerContext;
use crossbeam_channel::Receiver;
use ext_php_rs::embed::{ext_php_rs_sapi_per_thread_init, ext_php_rs_sapi_per_thread_shutdown};
use ext_php_rs::ffi::{
    ZEND_RESULT_CODE_SUCCESS, php_execute_script, php_request_shutdown, php_request_startup,
    zend_destroy_file_handle, zend_file_handle, zend_stream_init_filename,
};
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::thread::JoinHandle;

struct WorkerGuard;

impl WorkerGuard {
    pub fn new(worker_id: u32) -> Self {
        unsafe { ext_php_rs_sapi_per_thread_init() }

        ServerContext::init(worker_id);

        Self
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        ServerContext::destroy();

        unsafe {
            ext_php_rs_sapi_per_thread_shutdown();
        }
    }
}

pub struct Worker {
    handle: JoinHandle<()>,
}

impl Worker {
    pub fn new(id: u32, rx: Receiver<Job>) -> Self {
        let handle = std::thread::spawn(move || {
            let _guard = WorkerGuard::new(id);

            loop {
                let Ok(job) = rx.recv() else {
                    println!("Worker {} shutting down", id);
                    break;
                };

                ServerContext::start_request(job.respond_to);

                unsafe {
                    if php_request_startup() != ZEND_RESULT_CODE_SUCCESS {
                        panic!("request startup failed");
                    }

                    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                    path.push("src/php/test.php");

                    if !path.exists() {
                        panic!("file does not exist");
                    }

                    let filename_ptr = CString::new(path.as_os_str().as_encoded_bytes())
                        .unwrap()
                        .into_raw();

                    let mut file_handle = MaybeUninit::<zend_file_handle>::uninit();

                    zend_stream_init_filename(file_handle.as_mut_ptr(), filename_ptr);

                    let mut file_handle = file_handle.assume_init();
                    file_handle.primary_script = true;

                    if !php_execute_script(&mut file_handle) {
                        panic!("error executing script");
                    }

                    zend_destroy_file_handle(&mut file_handle);

                    php_request_shutdown(std::ptr::null_mut());

                    ServerContext::finish();

                    let _ = CString::from_raw(filename_ptr);
                }
            }
        });

        Self { handle }
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.handle.join()
    }
}
