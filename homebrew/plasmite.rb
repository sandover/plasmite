class Plasmite < Formula
  desc "Plasmite CLI"
  homepage "https://github.com/sandover/plasmite"
  license "MIT"
  version "0.1.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/sandover/plasmite/releases/download/v0.1.0/plasmite_0.1.0_darwin_arm64.tar.gz"
      sha256 "a6d477c3a19b8a0b84c6331361b4286144a97a53e860f47f0b11061d45d599a9"
    else
      url "https://github.com/sandover/plasmite/releases/download/v0.1.0/plasmite_0.1.0_darwin_amd64.tar.gz"
      sha256 "f0f618dac94b7ad62e90fea79c0bac54be2b7c61cd420d31b852a0b10ec7d432"
    end
  end

  on_linux do
    url "https://github.com/sandover/plasmite/releases/download/v0.1.0/plasmite_0.1.0_linux_amd64.tar.gz"
    sha256 "74b60da6df31345580c4c805c187d47c6b5bef295fa9ba7cb13e08ba429a80c8"
  end

  def install
    bin.install "plasmite"
    bin.install "pls"
  end

  test do
    assert_match "plasmite", shell_output("#{bin}/plasmite")
  end
end
