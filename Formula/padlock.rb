class Padlock < Formula
  desc "Struct memory layout analyzer for C, C++, Rust, Go, and Zig"
  homepage "https://github.com/gidotencate/padlock"
  url "https://github.com/gidotencate/padlock/archive/refs/tags/v0.9.5.tar.gz"
  # sha256 computed from the release tarball — update on each version bump:
  #   curl -sL https://github.com/gidotencate/padlock/archive/refs/tags/v0.9.5.tar.gz | shasum -a 256
  sha256 "PLACEHOLDER_UPDATE_WITH_REAL_SHA256"
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/gidotencate/padlock.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/padlock-cli")
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
