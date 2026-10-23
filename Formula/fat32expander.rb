# Homebrew formula for fat32expander. This repository is its own tap.
#
# The release build rewrites the version line, the four sha256 lines by their
# marker comments and the bottle block between the BOTTLE markers. Keep the
# markers, and keep each url line directly above its sha256 line.
class Fat32expander < Formula
  desc "Grow a FAT32 filesystem in place after its partition was enlarged"
  homepage "https://github.com/oetiker/fat32expander"
  version "0.1.0"
  license "MIT"

  # BOTTLE-START
  bottle do
    root_url "https://github.com/oetiker/fat32expander/releases/download/v0.1.0"
    sha256 cellar: :any_skip_relocation, arm64_sonoma: "b4f1f4afdb877c3157d5121e91451a0ed1ca7fb70d6a5a776d2d36f872342e52"
  end
  # BOTTLE-END

  on_macos do
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "e36a6d58b9fbd2e8308a49e05b415280ec059e3e86d411fb232d19d8b370bb6f" # mac-arm
    end
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "1e110143bea61e43be0d404297a999c05802c8c3e8f27047a847cd9196e9853f" # mac-x86
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "668e7032461876c4bd895261f2704001fb0cb82adbaaddb8515723192ce2da22" # linux-x86
    end
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "88f7fb4da86ae49e97f88a27ee156348bf66ccf64cdf44316dfdddbf1497d0ea" # linux-arm
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
