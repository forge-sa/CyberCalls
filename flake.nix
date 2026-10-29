{
  description = "CyberCalls — Edgerunners-style call overlay for AyuGram";

  # "nixpkgs" resolves to the system-pinned nixpkgs, so the shell reuses what's already in the store
  inputs.nixpkgs.url = "nixpkgs";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # dlopen'd at runtime by wgpu / wayland-client
      runtimeLibs = with pkgs; [ wayland libxkbcommon vulkan-loader ];
    in {
      devShells.${system}.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs; [
          rustc cargo clippy rustfmt rust-analyzer
          rustPlatform.bindgenHook # LIBCLANG_PATH + clang args for pipewire-sys / whisper-rs-sys
          pkg-config cmake
          shaderc # glslc, for whisper.cpp's Vulkan shaders
          vulkan-tools
        ];
        buildInputs = with pkgs; [
          pipewire
          wayland
          libxkbcommon
          vulkan-loader
          vulkan-headers
        ];
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
        # stdenv strips -march=native by default; without it whisper.cpp loses AVX2/FMA and runs ~100x slower
        NIX_ENFORCE_NO_NATIVE = "0";
        GGML_NATIVE = "ON";
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    };
}
