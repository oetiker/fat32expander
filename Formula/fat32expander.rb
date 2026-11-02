# Homebrew formula for fat32expander. This repository is its own tap.
#
# The release build rewrites the version line, the four sha256 lines by their
# marker comments and the bottle block between the BOTTLE markers. Keep the
# markers, and keep each url line directly above its sha256 line.
class Fat32expander < Formula
  desc "Grow a FAT32 filesystem in place after its partition was enlarged"
  homepage "https://github.com/oetiker/fat32expander"
  version "0.1.2"
  license "MIT"

  # BOTTLE-START
  bottle do
    root_url "https://github.com/oetiker/fat32expander/releases/download/v0.1.2"
    sha256 cellar: :any_skip_relocation, arm64_big_sur: "2a2420acc6914e645432b357a67757035bac72016ecde45afba71b7208dedee8"
  end
  # BOTTLE-END

  on_macos do
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "ddd5a8eb9058de0a6703edefa822693a3f071c8870e966e0f78dd3f962435013" # mac-arm
    end
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "fc811d7a19587937d8b85e5eedd8173eae244487df6ca5b16d2e5c90532220cd" # mac-x86
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "8f0775f2b41b404f3ba32774b72cd699d2b0898d6fd9d975dd7f73f4bad8ed75" # linux-x86
    end
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "79085988430b5cdeeeb785ec23ac6699d0a03dfc4fadf22fce7dd0b98270cd85" # linux-arm
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
