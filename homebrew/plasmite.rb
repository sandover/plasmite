class Plasmite < Formula
  desc "Plasmite CLI"
  homepage "https://github.com/YOUR_GITHUB/plasmite"
  license "MIT"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/YOUR_GITHUB/plasmite/releases/download/v0.1.0/plasmite-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME"
    else
      url "https://github.com/YOUR_GITHUB/plasmite/releases/download/v0.1.0/plasmite-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME"
    end
  end

  on_linux do
    url "https://github.com/YOUR_GITHUB/plasmite/releases/download/v0.1.0/plasmite-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "REPLACE_ME"
  end

  def install
    bin.install "plasmite"
  end

  test do
    assert_match "plasmite", shell_output("#{bin}/plasmite")
  end
end
