class Padlock < Formula
  desc "Struct memory layout analyzer for C, C++, Rust, Go, and Zig"
  homepage "https://github.com/gidotencate/padlock"
  version "0.80.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.80.0/padlock-v0.80.0-aarch64-apple-darwin.tar.gz"
      sha256 "d2ff1da2fadd13e2d76f29bd50f74a6f6a5b47ad9cbf534b0e8de10e92439bf2"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.80.0/padlock-v0.80.0-x86_64-apple-darwin.tar.gz"
      sha256 "9ea91fca613e1b3bdd2e2d05a74338154d7bc268fd476f3f57a05e77c597aa93"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.80.0/padlock-v0.80.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f9e34b87c56d7306de6d2169385e60e5fd9893a385255f229481574c12b99ac6"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.80.0/padlock-v0.80.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "8e53ed2e98d3aa45a0dcabe259fe44f442fe87538df85d8d16afc574cb428552"
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
