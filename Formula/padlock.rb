class Padlock < Formula
  desc "Struct memory layout analyzer for C, C++, Rust, Go, and Zig"
  homepage "https://github.com/gidotencate/padlock"
  version "0.81.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.81.0/padlock-v0.81.0-aarch64-apple-darwin.tar.gz"
      sha256 "84c0252ce7fe4e202051d26c8fc5d83e82737ddeb7199109c314af64b4b596b9"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.81.0/padlock-v0.81.0-x86_64-apple-darwin.tar.gz"
      sha256 "5adf8a84a1d02ba1aceb0b4ea6ede766b48ee420750b3c3bbe3e9f9e6df5f335"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.81.0/padlock-v0.81.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "a369e21b8b89afd3bdfe395a57efac241e4976f096d81c759c3183e5bf0a018a"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.81.0/padlock-v0.81.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "bd432c55f810620530767bd82e211cfa0016a67264cfe4d4c5561e8def6bc97d"
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
