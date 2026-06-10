use crate::cli::Config;
use ext_php_rs::boxed::ZBox;
use ext_php_rs::types::ZendStr;
use hyper::header::{
    ACCEPT, ACCEPT_ENCODING, ACCEPT_LANGUAGE, AUTHORIZATION, CACHE_CONTROL, CONNECTION,
    CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, DATE, HOST, HeaderName, ORIGIN,
    REFERER, USER_AGENT,
};
use std::sync::OnceLock;

macro_rules! interned_server_vars {
    (
        vars {
            $(
                $field:ident => $value:literal
            ),* $(,)?
        }

        runtime_vars {
            $(
                $runtime_field:ident => $runtime_expr:expr
            ),* $(,)?
        }

        standard_headers {
            $(
                $header_const:ident => $standard_target:ident
            ),* $(,)?
        }

        custom_headers {
            $(
                $custom_header:literal => $custom_target:ident
            ),* $(,)?
        }
    ) => {
        pub struct Interned {
            $(
                pub $field: ZBox<ZendStr>,
            )*

            $(
                pub $runtime_field: ZBox<ZendStr>,
            )*
        }

        unsafe impl Send for Interned {}
        unsafe impl Sync for Interned {}

        impl Interned {
            pub unsafe fn init(config: &Config) -> Self {
                Self {
                    $(
                        $field: ZendStr::new_interned($value, true),
                    )*

                    $(
                        $runtime_field: ZendStr::new_interned(
                            $runtime_expr(config),
                            true
                        ),
                    )*
                }
            }

            #[inline(always)]
            pub fn map_header_name(
                &self,
                name: &HeaderName,
            ) -> Option<&ZBox<ZendStr>> {

                // FAST PATH
                match *name {
                    $(
                        $header_const => {
                            return Some(&self.$standard_target)
                        }
                    )*
                    _ => {}
                }

                // FALLBACK PATH
                match name.as_str() {
                    $(
                        $custom_header => {
                            Some(&self.$custom_target)
                        }
                    )*
                    _ => None,
                }
            }
        }
    };
}

interned_server_vars! {
    vars {
        // Application identity
        ferrumphp => "FerrumPHP",
        server_software => "SERVER_SOFTWARE",
        gateway_interface => "GATEWAY_INTERFACE",
        cgi11 => "CGI/1.1",

        // Request metadata
        request_uri => "REQUEST_URI",
        request_method => "REQUEST_METHOD",
        query_string => "QUERY_STRING",
        server_protocol => "SERVER_PROTOCOL",

        // Server identity
        server_name => "SERVER_NAME",
        server_port => "SERVER_PORT",
        server_addr => "SERVER_ADDR",
        script_name => "SCRIPT_NAME",
        script_filename => "SCRIPT_FILENAME",
        document_root => "DOCUMENT_ROOT",
        path_info => "PATH_INFO",
        path_translated => "PATH_TRANSLATED",
        php_self => "PHP_SELF",

        // Client identity
        remote_addr => "REMOTE_ADDR",
        remote_port => "REMOTE_PORT",

        // Security
        https => "HTTPS",

        // Standard HTTP_* header keys
        http_accept => "HTTP_ACCEPT",
        http_accept_encoding => "HTTP_ACCEPT_ENCODING",
        http_accept_language => "HTTP_ACCEPT_LANGUAGE",
        http_authorization => "HTTP_AUTHORIZATION",
        http_cache_control => "HTTP_CACHE_CONTROL",
        http_connection => "HTTP_CONNECTION",
        http_content_encoding => "HTTP_CONTENT_ENCODING",
        http_cookie => "HTTP_COOKIE",
        http_date => "HTTP_DATE",
        http_host => "HTTP_HOST",
        http_origin => "HTTP_ORIGIN",
        http_referer => "HTTP_REFERER",
        http_user_agent => "HTTP_USER_AGENT",
        content_type => "CONTENT_TYPE",
        content_length => "CONTENT_LENGTH",

        // Non-standard HTTP_* header keys
        http_x_forwarded_for => "HTTP_X_FORWARDED_FOR",
        http_x_forwarded_host => "HTTP_X_FORWARDED_HOST",
        http_x_forwarded_proto => "HTTP_X_FORWARDED_PROTO",
        http_x_real_ip => "HTTP_X_REAL_IP",
        http_x_request_id => "HTTP_X_REQUEST_ID",
    }

    runtime_vars {
        server_addr_value =>
            |cfg: &Config| cfg.bind.ip().to_string(),
        server_port_value =>
            |cfg: &Config| cfg.bind.port().to_string(),
        script_filename_value =>
            |cfg: & Config| cfg.entrypoint.to_str().unwrap().to_string(),
        document_root_value =>
            |cfg: & Config| {
            let mut entry = cfg.entrypoint.clone();
            entry.pop(); // todo check if returns false

            entry.to_str().unwrap().to_string()
        },
        script_name_value => |cfg: & Config| {
            let mut doc_root = cfg.entrypoint.clone();
            doc_root.pop(); // todo check if returns false

            let doc_root = doc_root.to_str().unwrap();

            cfg.entrypoint.to_str().unwrap().strip_prefix(doc_root).unwrap().to_string()
        },

    }

    standard_headers {
        ACCEPT => http_accept,
        ACCEPT_ENCODING => http_accept_encoding,
        ACCEPT_LANGUAGE => http_accept_language,
        AUTHORIZATION => http_authorization,
        CACHE_CONTROL => http_cache_control,
        CONNECTION => http_connection,
        CONTENT_ENCODING => http_content_encoding,
        CONTENT_LENGTH => content_length,
        CONTENT_TYPE => content_type,
        COOKIE => http_cookie,
        DATE => http_date,
        HOST => http_host,
        ORIGIN => http_origin,
        REFERER => http_referer,
        USER_AGENT => http_user_agent,
    }

    custom_headers {
        "x-forwarded-for" => http_x_forwarded_for,
        "x-forwarded-host" => http_x_forwarded_host,
        "x-forwarded-proto" => http_x_forwarded_proto,
        "x-real-ip" => http_x_real_ip,
        "x-request-id" => http_x_request_id,
    }
}

pub static INTERNED: OnceLock<Interned> = OnceLock::new();
