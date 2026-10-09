class Padlock < Formula
  desc "Struct memory layout analyzer for C, C++, Rust, Go, and Zig"
  homepage "https://github.com/gidotencate/padlock"
  version "0.11.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.11.0/padlock-v0.11.0-aarch64-apple-darwin.tar.gz"
      sha256 "12eda5449093106f1f716d313771be1408cb8fc38429d4d8b92ae0a2b6553a12"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.11.0/padlock-v0.11.0-x86_64-apple-darwin.tar.gz"
      sha256 "5c1261c87a95c8cbec0d1945e10ce0ca954a344a292b1aa5d8135dc09e4aa499"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.11.0/padlock-v0.11.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e9aa07d586935cd8779f8db14bd070c0233f2b3e97f9c6984d230c9e5856ddc6"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.11.0/padlock-v0.11.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "4a99478e9ae5cfad725119c36cd2ba11012a088ff169df425dc30bea6237a05b"
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
