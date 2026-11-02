# Homebrew formula for fat32expander. This repository is its own tap.
#
# The release build rewrites the version line, the four sha256 lines by their
# marker comments and the bottle block between the BOTTLE markers. Keep the
# markers, and keep each url line directly above its sha256 line.
class Fat32expander < Formula
  desc "Grow a FAT32 filesystem in place after its partition was enlarged"
  homepage "https://github.com/oetiker/fat32expander"
  version "0.1.1"
  license "MIT"

  # BOTTLE-START
  bottle do
    root_url "https://github.com/oetiker/fat32expander/releases/download/v0.1.1"
    sha256 cellar: :any_skip_relocation, arm64_big_sur: "87a9ae663fa6282013d984259ec0f35778b4eb4fbc5bec0967b13495b8199c3a"
  end
  # BOTTLE-END

  on_macos do
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "3ce2b959f35baadb4e7dd3a6202f47b4b4fb39dc460a0d8d6f4bd274fdc2f38d" # mac-arm
    end
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "77a0d1836c507881e47af68f97e5406a4424ce073c055f1981ff8c1de6b4adcb" # mac-x86
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "84e3e346eb898f02abfa1bf0d72ad9904dc3b786cefa56dfc6361f7393fbcab6" # linux-x86
    end
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "097c9c2bf18506c7128d8accec808be68f969d00d112ba62ae18bd3b2c8803bc" # linux-arm
    end
  end

  def install
    bin.install "fat32expander"
    Dir["man/*.[1-9]"].each { |page| (man/"man#{page[-1]}").install page }
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/fat32expander --version")
  end
end
