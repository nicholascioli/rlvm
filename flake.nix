{
  inputs = {
    naersk.url = "github:nix-community/naersk/master";
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils, naersk }:
    utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        naersk-lib = pkgs.callPackage naersk { };

        csi-sanity = pkgs.buildGoModule rec {
          pname = "csi-sanity";
          version = "5.0.0";

          src = pkgs.fetchFromGitHub {
            owner = "kubernetes-csi";
            repo = "csi-test";
            rev = "v${version}";
            sha256 = "sha256-wQ2GKgdUOQc8tFwmuo40rMNY52KOuV5a5GXEcCy9zig=";
          };

          subPackages = [ "cmd/csi-sanity" ];

          deleteVendor = true;
          vendorSha256 = "sha256-NfkFiNGtpbH4frhV6FCp8SffZMbkn36WBbQQ6XuAqxY=";
        };
      in
      {
        defaultPackage = naersk-lib.buildPackage {
          src = ./.;
          gitSubmodules = true;
          copyLibs = true;

          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ linuxHeaders.out protobuf ];

          LIBCLANG_PATH = "${pkgs.llvmPackages_11.libclang.lib}/lib";
          BINDGEN_EXTRA_CLANG_ARGS = "
            ${pkgs.lib.readFile "${pkgs.stdenv.cc}/nix-support/libc-crt1-cflags"} \
            ${pkgs.lib.readFile "${pkgs.stdenv.cc}/nix-support/libc-cflags"} \
            ${pkgs.lib.readFile "${pkgs.stdenv.cc}/nix-support/cc-cflags"} \
            ${pkgs.lib.readFile "${pkgs.stdenv.cc}/nix-support/libcxx-cxxflags"} \
            ${pkgs.lib.optionalString pkgs.stdenv.cc.isClang "-idirafter ${pkgs.stdenv.cc.cc}/lib/clang/${pkgs.lib.getVersion pkgs.stdenv.cc.cc}/include"} \
            ${pkgs.lib.optionalString pkgs.stdenv.cc.isGNU "-isystem ${pkgs.stdenv.cc.cc}/include/c++/${pkgs.lib.getVersion pkgs.stdenv.cc.cc} -isystem ${pkgs.stdenv.cc.cc}/include/c++/${pkgs.lib.getVersion pkgs.stdenv.cc.cc}/${pkgs.stdenv.hostPlatform.config} -idirafter ${pkgs.stdenv.cc.cc}/lib/gcc/${pkgs.stdenv.hostPlatform.config}/${pkgs.lib.getVersion pkgs.stdenv.cc.cc}/include"}
          ";
        };
        devShell = with pkgs; mkShell {
          buildInputs = [ cargo rustc rustfmt pre-commit rustPackages.clippy ] ++ [
            # Tooling for testing
            grpcurl
            csi-sanity
            jq
            nixfmt
          ];
          RUST_SRC_PATH = rustPlatform.rustLibSrc;
        };
      });
}
