# Homebrew formula for tafas. Third-party tap: the repository is not named
# `homebrew-tap`, so the tap command names the URL explicitly:
#
#   brew tap RUdolf1517/tafas https://github.com/RUdolf1517/tafas
#   brew install RUdolf1517/tafas/tafas
#
# The `sha256` values are filled in from the release's SHA256SUMS after the tag
# exists; the release notes carry them. See docs/releases.md §4.
class Tafas < Formula
  desc "Capture a video from YouTube, TikTok, X, Vimeo, Reddit, SoundCloud or Twitch as one playable MP4"
  homepage "https://github.com/RUdolf1517/tafas"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/RUdolf1517/tafas/releases/download/v#{version}/tafas-x86_64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "tafas"
    man1.install "man/tafas.1"
    # Completions are generated from the live parser, so they cannot drift from
    # `--help` the way a checked-in script can.
    generate_completions_from_executable(bin/"tafas", "--completions")
  end

  def caveats
    <<~EOS
      tafas writes no configuration file and keeps no state between runs.
      Nothing else needs to be installed: it does not call ffmpeg, yt-dlp or any
      other program.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/tafas --version")
    assert_match "\"sites\"", shell_output("#{bin}/tafas --capabilities")
    # A usage error, made without a network request: exit 2, nothing fetched.
    assert_match "usage:", shell_output("#{bin}/tafas --quality nope https://youtu.be/x 2>&1", 2)
  end
end
