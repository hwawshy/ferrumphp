fn main() {
    println!("cargo:rustc-link-search=/opt/php-8.5-embed-zts-debug/lib");
    println!("cargo:rustc-link-lib=dylib=php");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/opt/php-8.5-embed-zts-debug/lib");
}
