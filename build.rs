use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rustc-link-search=/opt/php-8.5-embed-zts-debug/lib");
    println!("cargo:rustc-link-lib=dylib=php");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/opt/php-8.5-embed-zts-debug/lib");
    println!("cargo:rerun-if-changed=build/ferrumphp.h");
    println!("cargo:rerun-if-changed=build/ferrumphp.c");

    let php_config = find_php_config().expect("Unable to find php-config");
    let cmd = Command::new(php_config).arg("--includes").output()?;
    let includes = String::from_utf8_lossy(&cmd.stdout);

    let include_paths: Vec<_> = includes
        .split(' ')
        .filter_map(|part| part.strip_prefix("-I"))
        .collect();

    cc::Build::new()
        .compiler("clang")
        .define("ZTS", "1")
        .file("build/ferrumphp.c")
        .includes(include_paths)
        .try_compile("ferrumphp")?;

    let builder = bindgen::Builder::default()
        .header("build/ferrumphp.h")
        .clang_args(includes.split(' '))
        .clang_arg("-DZTS=1")
        .allowlist_function("sapi_send_headers")
        .allowlist_function("ferrumphp_error");

    let bindings = builder.generate()?;
    let out_path = PathBuf::from(std::env::var("OUT_DIR")?);
    bindings.write_to_file(out_path.join("bindings.rs"))?;

    Ok(())
}

// taken from ext-php-rs
fn find_php_config() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PHP_CONFIG").map(PathBuf::from) {
        if path.try_exists().ok()? {
            return Some(path);
        }
    }

    None
}
