set -e
sudo echo sudo
cargo b -r
sudo just install
#killall cosmic-panel
cosmic-panel
