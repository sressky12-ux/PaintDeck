# 訓練小幫手（resources/train/agent.py）的協定測試。用假的訓練指令，不需要顯卡、不需要 ai-toolkit。
# 跑法：python -I tests/train_agent_test.py
import io
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
AGENT = os.path.join(HERE, "..", "resources", "train", "agent.py")
TOKEN = "test-token-123"

FAKE = r"""
import os, sys, time
print("caching latents 3/6", flush=True)
sys.stdout.write("lora:  10%|#  | 10/100 [00:01<00:09, lr: 1.0e-04 loss: 1.2e-01]\r")
sys.stdout.flush()
time.sleep(float(os.environ.get("FAKE_SLEEP", "1.5")))
out = os.path.join(os.environ["TRAIN_OUT"], "lora")
os.makedirs(os.path.join(out, "samples"), exist_ok=True)
open(os.path.join(out, "lora_000000050.safetensors"), "wb").write(b"FAKE-LORA-" + bytes(range(256)))
open(os.path.join(out, "samples", "1_0.jpg"), "wb").write(b"jpg")
os.makedirs(os.path.join(out, "samples", ".thumbs"), exist_ok=True)
open(os.path.join(out, "samples", ".thumbs", "1_0.jpg.jpg"), "wb").write(b"thumb")
assert os.path.isfile(sys.argv[1]), "config 沒寫出來"
assert os.path.isfile(os.path.join(os.environ["TRAIN_DATA"], "0001.txt")), "資料集沒解開"
sys.exit(int(os.environ.get("FAKE_RC", "0")))
"""


class AgentTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.work = os.path.join(self.tmp.name, "work")
        fake = os.path.join(self.tmp.name, "fake_train.py")
        with open(fake, "w", encoding="utf-8") as f:
            f.write(FAKE)
        env = dict(os.environ, TRAIN_CMD=json.dumps([sys.executable, "-I", fake, "{config}"]), PYTHONUTF8="1")
        env.update(getattr(self, "extra_env", {}))
        self.proc = subprocess.Popen(
            [sys.executable, "-I", AGENT, "--port", "0", "--token", TOKEN, "--work", self.work, "--toolkit", self.tmp.name] + list(getattr(self, "extra_args", [])),
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env, text=True, encoding="utf-8",
        )
        line = self.proc.stdout.readline()
        self.assertIn("TRAIN_AGENT_READY", line, line)
        self.base = "http://127.0.0.1:" + line.strip().split("port=")[1]

    def tearDown(self):
        self.proc.kill()
        self.proc.wait(timeout=10)
        self.proc.stdout.close()
        self.tmp.cleanup()

    def call(self, method, path, body=None, token=TOKEN, raw=False):
        data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
        req = urllib.request.Request(self.base + path, data=data, method=method)
        if token is not None:
            req.add_header("X-Train-Token", token)
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                out = r.read()
                return r.status, (out if raw else json.loads(out or b"{}"))
        except urllib.error.HTTPError as e:
            out = e.read()
            e.close()
            return e.code, (out if raw else json.loads(out or b"{}"))

    @staticmethod
    def zip_of(files):
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            for name, data in files.items():
                z.writestr(name, data)
        return buf.getvalue()

    def wait_state(self, want, timeout=15):
        end = time.time() + timeout
        while time.time() < end:
            st = self.call("GET", "/status")[1]
            if st["state"] in want:
                return st
            time.sleep(0.2)
        self.fail(f"等不到 {want}：{st}")

    def test_wrong_token_is_401(self):
        self.assertEqual(self.call("GET", "/status", token="nope")[0], 401)
        self.assertEqual(self.call("GET", "/status", token=None)[0], 401)
        self.assertEqual(self.call("POST", "/start", {"config": "x"}, token="nope")[0], 401)
        code, st = self.call("GET", "/status")
        self.assertEqual((code, st["state"], st["step"]), (200, "idle", 0))
        self.assertTrue(os.path.isabs(st["data_dir"]) and os.path.isabs(st["out_dir"]))

    def test_zip_with_parent_path_is_rejected(self):
        code, r = self.call("POST", "/dataset", self.zip_of({"0001.png": b"x", "../evil.txt": b"boom"}))
        self.assertEqual(code, 400, r)
        self.assertFalse(os.path.exists(os.path.join(self.work, "evil.txt")))
        self.assertFalse(os.path.exists(os.path.join(self.tmp.name, "evil.txt")))
        code, r = self.call("POST", "/dataset", b"this is not a zip")
        self.assertEqual(code, 400, r)

    def test_full_run(self):
        code, r = self.call("POST", "/dataset", self.zip_of({"0001.png": b"png", "0001.txt": "xk, 1girl".encode()}))
        self.assertEqual((code, r["files"]), (200, 2), r)
        # 再傳一次會先清空舊的
        code, r = self.call("POST", "/dataset", self.zip_of({"0001.png": b"png", "0001.txt": b"t", "0002.png": b"p"}))
        self.assertEqual((code, r["files"]), (200, 3), r)
        code, r = self.call("POST", "/start", {"config": "job: extension\n", "name": "lora"})
        self.assertEqual(code, 200, r)
        code, r = self.call("POST", "/start", {"config": "job: extension\n", "name": "lora"})
        self.assertEqual(code, 409, r)
        st = self.wait_state({"running"})
        end = time.time() + 5
        while st["step"] != 10 and time.time() < end:
            time.sleep(0.1)
            st = self.call("GET", "/status")[1]
        self.assertEqual((st["step"], st["total"]), (10, 100), st)
        self.assertAlmostEqual(st["loss"], 0.12)
        st = self.wait_state({"done", "error"})
        self.assertEqual(st["state"], "done", st)
        kinds = {f["name"]: f["kind"] for f in st["files"]}
        self.assertEqual(kinds, {"lora/lora_000000050.safetensors": "lora", "lora/samples/1_0.jpg": "sample"})
        self.assertTrue(any("caching latents" in line for line in st["log"]), st["log"])
        code, data = self.call("GET", "/file?name=lora/lora_000000050.safetensors", raw=True)
        self.assertEqual((code, data), (200, b"FAKE-LORA-" + bytes(range(256))))
        for bad in ["../agent.py", "..%2Fconfig.yaml", "C:/Windows/win.ini", "/etc/passwd", "lora/nope.safetensors"]:
            self.assertIn(self.call("GET", "/file?name=" + bad, raw=True)[0], (400, 404), bad)
        self.assertEqual(self.call("GET", "/file?name=../config.yaml", raw=True)[0], 400)
        # 跑完可以再開一次
        self.assertEqual(self.call("POST", "/start", {"config": "job: extension\n", "name": "lora"})[0], 200)
        self.wait_state({"done"})

    def test_stop_kills_training(self):
        self.call("POST", "/dataset", self.zip_of({"0001.png": b"png", "0001.txt": b"t"}))
        self.assertEqual(self.call("POST", "/start", {"config": "steps: 100\n", "name": "lora"})[0], 200)
        self.wait_state({"running"})
        self.assertEqual(self.call("POST", "/stop")[0], 200)
        st = self.wait_state({"stopped"})
        self.assertEqual(st["files"], [])
        self.assertEqual(self.call("POST", "/stop")[0], 200)


class AgentFailTest(AgentTest):
    extra_env = {"FAKE_RC": "3", "FAKE_SLEEP": "0.1"}

    def test_failed_training_reports_error(self):
        self.call("POST", "/dataset", self.zip_of({"0001.png": b"png", "0001.txt": b"t"}))
        self.assertEqual(self.call("POST", "/start", {"config": "steps: 100\n", "name": "lora"})[0], 200)
        st = self.wait_state({"done", "error"})
        self.assertEqual(st["state"], "error", st)
        self.assertIn("3", st["error"])

    test_wrong_token_is_401 = None
    test_zip_with_parent_path_is_rejected = None
    test_full_run = None
    test_stop_kills_training = None


class AgentWatchdogTest(AgentTest):
    """繪匣被關掉（沒人再來問進度）的時候，小幫手要自己把訓練停掉、結束，不能留孤兒占著顯卡。"""
    extra_args = ["--watchdog", "2"]
    extra_env = {"FAKE_SLEEP": "30"}

    def test_exits_when_nobody_asks(self):
        self.call("POST", "/dataset", self.zip_of({"0001.png": b"png", "0001.txt": b"t"}))
        self.assertEqual(self.call("POST", "/start", {"config": "steps: 100\n", "name": "lora"})[0], 200)
        self.wait_state({"running"})
        # 有人在問就不能停
        for _ in range(6):
            time.sleep(0.5)
            self.assertEqual(self.call("GET", "/status")[1]["state"], "running")
        self.assertIsNone(self.proc.poll())
        # 沒人問了：幾秒內自己結束
        self.assertEqual(self.proc.wait(timeout=15), 0)

    test_wrong_token_is_401 = None
    test_zip_with_parent_path_is_rejected = None
    test_full_run = None
    test_stop_kills_training = None


if __name__ == "__main__":
    unittest.main(verbosity=1)
