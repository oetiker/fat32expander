# Homebrew formula for fat32expander. This repository is its own tap.
#
# The release build rewrites the version line, the four sha256 lines by their
# marker comments and the bottle block between the BOTTLE markers. Keep the
# markers, and keep each url line directly above its sha256 line.
class Fat32expander < Formula
  desc "Grow a FAT32 filesystem in place after its partition was enlarged"
  homepage "https://github.com/oetiker/fat32expander"
  version "0.0.2"
  license "MIT"

  # BOTTLE-START
  # BOTTLE-END

  on_macos do
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # mac-arm
    end
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # mac-x86
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # linux-x86
    end
    on_arm do
      url "https://github.com/oetiker/fat32expander/releases/download/v#{version}/fat32expander-#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000" # linux-arm
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
