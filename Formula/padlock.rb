class Padlock < Formula
  desc "Struct memory layout analyzer for C, C++, Rust, Go, and Zig"
  homepage "https://github.com/gidotencate/padlock"
  version "0.12.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.12.0/padlock-v0.12.0-aarch64-apple-darwin.tar.gz"
      sha256 "52ccbfac93c2c90575ef5d30c55cf41fff1ee0a9c7ff6a44d1ec8b84f61a41d0"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.12.0/padlock-v0.12.0-x86_64-apple-darwin.tar.gz"
      sha256 "c91c567a657816900c9c0039770cbd4017ddf63a52e5e4a7f32596360960b115"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.12.0/padlock-v0.12.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "3448a293dcda8a3a4f16df0f60cd962728e0f53adb16b88371ae2fc599aa8f30"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.12.0/padlock-v0.12.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "f66e94ae8e1ab0c86ea93a92d30f33fb62633e4e52b91a73c2a384bba49a6d99"
    end
  end

  def install
    bin.install "padlock"
    bin.install "cargo-padlock"
    bin.install "padlock-lsp"
  end

  test do
    # Basic smoke test — version flag must succeed
    assert_match "padlock #{version}", shell_output("#{bin}/padlock --version")

    # Write a minimal C struct and confirm padlock can analyse it
    (testpath/"test.c").write <<~C
      struct Padded {
          char   a;
          double b;
          char   c;
      };
    C
    output = shell_output("#{bin}/padlock analyze test.c --json")
    assert_match "Padded", output
  end
end
