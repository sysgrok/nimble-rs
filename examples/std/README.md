# `nimble-rs` STD (host) examples

These examples run the full `nimble-rs` stack on a **host** (Linux/macOS) using any Linux HCI controller.

## Running

The examples run over any Linux HCI controller - a real adapter or a virtual
one from BlueZ's `btvirt`:

```sh
sudo apt install bluez-test-tools     # provides btvirt
sudo modprobe hci_vhci
sudo btvirt -l2 &                     # two virtual LE controllers: hci0, hci1
```

The transport binds `HCI_CHANNEL_USER`, which needs the device *down* and
`CAP_NET_ADMIN` (run via `sudo`, or
`sudo setcap cap_net_admin+ep target/debug/<bin>`; for real adapters also
`sudo hciconfig hciX down` first - `btvirt` devices start down).

```sh
cargo build -p nimble-rs-examples-std

sudo ./target/debug/gatt_server 0     # advertise + serve GATT on hci0
sudo ./target/debug/gatt_client 1     # scan, connect and subscribe from hci1
sudo ./target/debug/scanner 0         # just scan
sudo ./target/debug/l2cap server 0    # L2CAP CoC echo server ...
sudo ./target/debug/l2cap client 1    # ... and its client
```

Watch the HCI traffic with `sudo btmon`.
