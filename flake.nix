{
  description = "FerrumPHP development environment";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
  };
  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      php = pkgs.php85.override { ztsSupport = true; embedSupport = true; };
      phpDev = php.unwrapped.dev;
    in
    {
      devShells.${system} = {
        default = pkgs.mkShell {
          buildInputs = [
            php
            phpDev
            pkgs.libclang.lib
            pkgs.clang
            pkgs.valgrind
          ];
          nativeBuildInputs = [
            pkgs.rustc
            pkgs.cargo
            pkgs.clippy
          ];

          PHP_INI_SCAN_DIR = "${php}/lib";

          shellHook = ''export LIBCLANG_PATH="${pkgs.libclang.lib}/lib" export BINDGEN_EXTRA_CLANG_ARGS="-resource-dir ${pkgs.libclang.lib}/lib/clang/${pkgs.lib.versions.major (pkgs.lib.getVersion pkgs.clang)} -isystem ${pkgs.glibc.dev}/include" '';
        };
      };
    };
}