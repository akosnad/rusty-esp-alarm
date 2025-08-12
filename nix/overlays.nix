final: prev: {
  rust-esp = prev.callPackage ./rust-esp.nix { };
  rust-src-esp = prev.callPackage ./rust-src-esp.nix { };

  esp-idf-esp32-with-clang = final.esp-idf-full.override {
    rev = "v5.3.1";
    sha256 = "sha256-hcE4Tr5PTRQjfiRYgvLB1+8sR7KQQ1TnQJqViodGdBw=";
    toolsToInclude = [
      "esp-clang"
      "xtensa-esp-elf"
      "esp32ulp-elf"
      "openocd-esp32"
      "xtensa-esp-elf-gdb"
    ];
  };
}
