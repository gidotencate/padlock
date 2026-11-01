class Padlock < Formula
  desc "Struct memory layout analyzer for C, C++, Rust, Go, and Zig"
  homepage "https://github.com/gidotencate/padlock"
  version "0.82.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.82.0/padlock-v0.82.0-aarch64-apple-darwin.tar.gz"
      sha256 "05ef3959b5ef87a041cf994c983fc0953f2bf07123bc11c4f926894f529ab270"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.82.0/padlock-v0.82.0-x86_64-apple-darwin.tar.gz"
      sha256 "87d021f48d59a46fdefdc062f9e9c7e3177f56ab26852d8b2e14f10fe37ba868"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.82.0/padlock-v0.82.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0d1369fb3594dfc7cf4e853997989208783f80c592a666a0a1e1b71ca61342c5"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.82.0/padlock-v0.82.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "ce3dfa42586e650535eacc3024a286e6d793522d92417a00ba1a3722ed744380"
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
