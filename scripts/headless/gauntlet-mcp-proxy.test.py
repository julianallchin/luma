"""The recorder must preserve the wire exactly and retain inspectable images."""
import base64
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


class RecorderTest(unittest.TestCase):
    def test_round_trip_and_image_evidence(self):
        with tempfile.TemporaryDirectory(prefix="luma-proxy-test-") as directory:
            root = pathlib.Path(directory)
            image = base64.b64encode(b"image-payload").decode()
            result = {"jsonrpc": "2.0", "id": 7, "result": {"content": [
                {"type": "text", "text": "complete response"},
                {"type": "image", "mimeType": "image/png", "data": image},
            ]}}
            server = root / "server.py"
            server.write_text("import sys\nline = sys.stdin.buffer.readline()\n"
                              f"assert line == {b'{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\"}\n'!r}\n"
                              f"sys.stdout.write({json.dumps(result) + chr(10)!r})\n")
            request = b'{"jsonrpc":"2.0","id":7,"method":"tools/call"}\n'
            completed = subprocess.run([
                sys.executable, str(pathlib.Path(__file__).with_name("gauntlet-mcp-proxy.py")),
                str(root / "evidence"), sys.executable, str(server),
            ], input=request, capture_output=True, timeout=10, check=True)
            self.assertEqual(completed.stdout, (json.dumps(result) + "\n").encode())
            trace = [json.loads(line) for line in (root / "evidence/mcp.jsonl").read_text().splitlines()]
            self.assertEqual([item["direction"] for item in trace], ["request", "response", "exit"])
            self.assertEqual(trace[0]["frame"], json.loads(request))
            self.assertEqual(trace[1]["frame"], result)
            images = list((root / "evidence/images").glob("*.png"))
            self.assertEqual(len(images), 1)
            self.assertEqual(images[0].read_bytes(), b"image-payload")


if __name__ == "__main__":
    unittest.main()
