"""Exercise the native viewer with synthetic mTLS video/control, no screen access."""
import argparse
import json
import pathlib
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time


def exact(stream, count):
    data = b""
    while len(data) < count:
        block = stream.recv(count - len(data))
        if not block:
            raise EOFError("unexpected disconnect")
        data += block
    return data


def receive(stream):
    return json.loads(exact(stream, struct.unpack("!I", exact(stream, 4))[0]))


def send(stream, message):
    body = json.dumps(message).encode()
    stream.sendall(struct.pack("!I", len(body)) + body)


def gray_jpeg(width, height):
    # Baseline RGB-compatible JPEG: three non-subsampled components, all DC=0.
    # One-bit Huffman codes for zero DC and EOB; no third-party image dependency.
    def segment(marker, body):
        return bytes([255, marker]) + struct.pack("!H", len(body) + 2) + body
    tables = b"\x00" + bytes([1] + [0] * 15) + b"\x00"
    tables += b"\x10" + bytes([1] + [0] * 15) + b"\x00"
    blocks = ((width + 7) // 8) * ((height + 7) // 8) * 3
    bit_count = blocks * 2
    entropy = bytearray((bit_count + 7) // 8)
    if bit_count % 8:
        entropy[-1] = (1 << (8 - bit_count % 8)) - 1
    return (b"\xff\xd8" + segment(0xdb, b"\x00" + bytes([1] * 64))
            + segment(0xc0, b"\x08" + struct.pack("!HH", height, width)
                      + b"\x03\x01\x11\x00\x02\x11\x00\x03\x11\x00")
            + segment(0xc4, tables)
            + segment(0xda, b"\x03\x01\x00\x02\x00\x03\x00\x00\x3f\x00")
            + entropy + b"\xff\xd9")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--test-mouse", action="store_true",
                        help="macOS local input regression check (requires Accessibility)")
    parser.add_argument("--test-keyboard", action="store_true",
                        help="exercise native key capture (macOS needs Accessibility)")
    args = parser.parse_args()
    binary = args.binary.resolve()
    with tempfile.TemporaryDirectory(prefix="myremoteapp-viewer-") as directory:
        root = pathlib.Path(directory)
        input_helper = root / "viewer-input"
        if args.test_mouse or (args.test_keyboard and sys.platform == "darwin"):
            subprocess.run(["xcrun", "swiftc", str(pathlib.Path(__file__).with_name("smoke_viewer_input.swift")),
                            "-o", str(input_helper)], check=True, capture_output=True)
        provision = subprocess.Popen(
            [str(binary), "serve", "--check-only", "--listen", "127.0.0.1:0",
             "--state-dir", str(root / "host")],
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            state_path = root / "host" / "identity.json"
            deadline = time.monotonic() + 15
            while True:
                if provision.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError("fixture identity provisioning failed")
                try:
                    json.loads((root / "host" / "connect.json").read_text())
                    identity = json.loads(state_path.read_text())
                    break
                except (FileNotFoundError, json.JSONDecodeError):
                    time.sleep(0.05)
        finally:
            if provision.poll() is None:
                provision.terminate()
            provision.communicate(timeout=10)
        connection = identity["connection"]
        for name, value in [("cert.pem", connection["host_certificate"]),
                            ("key.pem", identity["host_key"])]:
            path = root / name
            path.write_text(value)
            path.chmod(0o600)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_3
        context.load_cert_chain(root / "cert.pem", root / "key.pem")
        context.verify_mode = ssl.CERT_REQUIRED
        context.load_verify_locations(cadata=connection["client_certificate"])
        context.set_alpn_protocols(["myremoteapp-control/1"])
        errors, received = [], []
        video_done = threading.Event()
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(4)
        listener.settimeout(15)
        address = "%s:%s" % listener.getsockname()
        workers = []

        def handle(tcp):
            try:
                with context.wrap_socket(tcp, server_side=True) as stream:
                    stream.settimeout(15)
                    hello = receive(stream)
                    assert hello["type"] == "hello"
                    send(stream, {"type": "welcome", "version": hello["version"]})
                    command = receive(stream)
                    if command["type"] == "get_status":
                        send(stream, {"type": "status", "locked": False,
                                      "unlock_available": False})
                        assert receive(stream)["type"] == "goodbye"
                    elif command["type"] == "start_preview":
                        send(stream, {"type": "preview_started", "window": {
                            "id": 1, "app": "MyRemoteApp fixture", "title": "Native viewer test",
                            "width": 320, "height": 180}})
                        sequence = 0
                        for width, height in [(320, 180), (180, 320)]:
                            jpeg = gray_jpeg(width, height)
                            for _ in range(25 if args.test_keyboard else 10):
                                stream.sendall(struct.pack("!IIIQ", len(jpeg), width,
                                                           height, sequence) + jpeg)
                                sequence += 1
                                time.sleep(0.15)
                        stream.sendall(struct.pack("!I", 0))
                        video_done.set()
                    elif command == {"type": "start_control", "window_id": 1}:
                        send(stream, {"type": "control_ready"})
                        while True:
                            message = receive(stream)
                            received.append(message)
                            if message["type"] == "ping":
                                send(stream, {"type": "pong", "sequence": message["sequence"]})
                            elif message["type"] == "goodbye":
                                break
                    else:
                        raise AssertionError(command)
            except BaseException as error:
                errors.append(error)

        def server():
            try:
                for _ in range(3):
                    worker = threading.Thread(target=handle, args=(listener.accept()[0],), daemon=True)
                    workers.append(worker)
                    worker.start()
            except BaseException as error:
                errors.append(error)

        server_thread = threading.Thread(target=server, daemon=True)
        server_thread.start()
        try:
            client = subprocess.Popen(
                [str(binary), "connect", "--address", address, "--connection-file",
                 str(root / "host" / "connect.json")],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8")
            try:
                if sys.platform == "win32" and args.test_keyboard:
                    from smoke_viewer_keyboard_windows import send_test_keys
                    send_test_keys(client.pid)
                elif args.test_mouse or args.test_keyboard:
                    helper_args = [str(input_helper), str(client.pid)]
                    if args.test_keyboard:
                        helper_args.append("--keyboard")
                    subprocess.run(helper_args, check=True, capture_output=True, timeout=10)
                stdout, stderr = client.communicate(timeout=25)
                result = subprocess.CompletedProcess(client.args, client.returncode, stdout, stderr)
            finally:
                if client.poll() is None:
                    client.kill()
                    client.communicate()
            server_thread.join(timeout=2)
            for worker in workers:
                worker.join(timeout=2)
            assert not errors, errors
            assert video_done.is_set(), "synthetic video did not finish"
            assert "320×180" in result.stdout and "180×320" in result.stdout, result
            assert result.returncode == 1 and "采集已停止" in result.stderr, result
            kinds = [message["type"] for message in received]
            assert "ping" in kinds and "release_input" in kinds and "goodbye" in kinds, received
            if args.test_mouse:
                actions = [message["event"]["kind"] for message in received if message["type"] == "mouse"]
                for button in ["left", "right"]:
                    for down in [True, False]:
                        assert {"action": "button", "button": button, "down": down} in actions, actions
                print("Native mouse: focused macOS viewer sends left/right down and up over TLS.")
            if args.test_keyboard:
                keys = [(message["event"]["key"], message["event"]["down"])
                        for message in received if message["type"] == "key"]
                expected = [("left_ctrl", True), ("a", True), ("a", True),
                            ("a", False), ("left_ctrl", False),
                            ("escape", True), ("escape", False),
                            ("left_super", True), ("a", True), ("a", False),
                            ("left_super", False), ("left_super", True),
                            ("q", True), ("q", False), ("left_super", False), ("left_ctrl", True)]
                # User input can arrive before the fixture's injected sequence.
                # Require the entire injected sequence, in order and contiguous.
                assert any(keys[start:start + len(expected)] == expected
                           for start in range(len(keys) - len(expected) + 1)), keys
                assert "release_keyboard" in kinds, received
                print("Native keyboard: Ctrl chord, key repeat, Escape, Command/Windows key, ordered releases and focus-loss release passed.")
            print("Native viewer: landscape/portrait decode and presentation, authenticated control heartbeat, stop and input release passed.")
        finally:
            listener.close()


if __name__ == "__main__":
    main()
