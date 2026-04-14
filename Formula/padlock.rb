class Padlock < Formula
  desc "Struct memory layout analyzer for C, C++, Rust, Go, and Zig"
  homepage "https://github.com/gidotencate/padlock"
  version "0.9.5"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.9.5/padlock-v0.9.5-aarch64-apple-darwin.tar.gz"
      sha256 "ede14d94540346d8e32c2276d4f7e8fcdfcd5f8aa2f4a2359d70e82f37073dcf"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.9.5/padlock-v0.9.5-x86_64-apple-darwin.tar.gz"
      sha256 "dc86dc3a7c651df7a3ffcc22b934cfe20aeaff8d488d5bdeee3dddfdd587e74f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/gidotencate/padlock/releases/download/v0.9.5/padlock-v0.9.5-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "ba5469121c8bc541194da380f1a7092bab247b460d4227998fb549f81de0d945"
    end
    on_intel do
      url "https://github.com/gidotencate/padlock/releases/download/v0.9.5/padlock-v0.9.5-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "104ee368e335decf8c60190214a9e38aefbc811ae480c649a346bd9164026381"
    end
  end

  def install
    bin.install "padlock"
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
