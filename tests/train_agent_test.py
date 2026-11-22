# 訓練小幫手（resources/train/agent.py）的協定測試。用假的訓練指令，不需要顯卡、不需要 ai-toolkit。
# 跑法：python -I tests/train_agent_test.py
import io
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request
import uuid
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


class AgentServerTestCase(unittest.TestCase):
    def setUp(self):
        # Windows 受限帳號需要繼承暫存目錄的 ACL。
        self.tmp = Path(tempfile.gettempdir(), "paintdeck-test-" + uuid.uuid4().hex)
        self.tmp.mkdir(mode=0o777 if os.name == "nt" else 0o700)
        self.addCleanup(shutil.rmtree, self.tmp)
        self.work = os.path.join(self.tmp, "work")
        fake = os.path.join(self.tmp, "fake_train.py")
        with open(fake, "w", encoding="utf-8") as f:
            f.write(FAKE)
        env = dict(os.environ, TRAIN_CMD=json.dumps([sys.executable, "-I", fake, "{config}"]), PYTHONUTF8="1")
        env.update(getattr(self, "extra_env", {}))
        self.proc = subprocess.Popen(
            [sys.executable, "-I", AGENT, "--port", "0", "--token", TOKEN, "--work", self.work, "--toolkit", str(self.tmp)] + list(getattr(self, "extra_args", [])),
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=env, text=True, encoding="utf-8",
        )
        line = self.proc.stdout.readline()
        self.assertIn("TRAIN_AGENT_READY", line, line)
        self.base = "http://127.0.0.1:" + line.strip().split("port=")[1]

    def tearDown(self):
        self.proc.kill()
        self.proc.wait(timeout=10)
        self.proc.stdout.close()

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


class AgentTest(AgentServerTestCase):
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
        self.assertFalse(os.path.exists(os.path.join(self.tmp, "evil.txt")))
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


class KohakuAgentTest(AgentServerTestCase):
    phases = ("text", "latents", "train")
    extra_env = {"TRAIN_CMDS": json.dumps({
        phase: [sys.executable, "-I", "-u", phase + ".py", "{config}"]
        for phase in ("text", "latents", "train")
    })}
    train_line = "train:   1%| | 9/800 [00:52<1:14:59,  5.69s/step, loss=0.2341, lr=1.8e-05, s/it=5.75, vram=2.1/2.4/3.6G]"

    def setUp(self):
        super().setUp()
        self.repo = self.tmp / "假 repo"
        self.repo.mkdir()
        self.kohaku = {
            "kogine": str(self.repo / "unused-kogine"),
            "repo": str(self.repo),
            "data_config": "# 訓練資料\n",
            "train_config": "# 訓練步數\n  STEPS = 800\n",
        }

    def start_kohaku(self, **scripts):
        for phase in self.phases:
            (self.repo / (phase + ".py")).write_text(
                "from pathlib import Path\nimport sys, os, time\n" + scripts.get(phase, ""), encoding="utf-8")
        req = {"engine": "kohakumoss", "name": "xk", "kohaku": self.kohaku}
        code, body = self.call("POST", "/start", req)
        self.assertEqual(code, 200, body)

    def wait_log(self, line):
        end = time.monotonic() + 10
        while time.monotonic() < end:
            st = self.call("GET", "/status")[1]
            if line in st["log"]:
                return st
            self.assertEqual(st["state"], "running", st)
            time.sleep(0.05)
        self.fail(f"等不到 {line}：{st}")

    def test_three_phases_in_order(self):
        scripts = {}
        for phase in self.phases:
            cfg = "train" if phase == "train" else "data"
            path = Path(self.work, "kohaku", cfg + ".py")
            scripts[phase] = (
                f"assert Path(sys.argv[1]) == Path({str(path)!r})\n"
                f"assert Path(sys.argv[1]).read_text(encoding='utf-8') == {self.kohaku[cfg + '_config']!r}\n"
                "with open('order', 'a') as f:\n"
                f"    print({phase!r}, file=f)\n"
            )
        self.start_kohaku(**scripts)
        st = self.wait_state({"done", "error"})
        self.assertEqual((st["state"], st["engine"], st["phase"], st["phase_index"]),
                         ("done", "kohakumoss", "train", 3), st)
        self.assertEqual((self.repo / "order").read_text().splitlines(), list(self.phases))

    def test_progress_and_loss_from_kohaku_line(self):
        # 相同分母也不能誤認。用檔案放行，才能確定讀到的是當段的狀態。
        gate = ("\nend = time.monotonic() + 10\n"
                "while not Path(__file__).with_suffix('.go').exists() and time.monotonic() < end:\n"
                "    time.sleep(0.03)\n")
        self.start_kohaku(text="print('caching 3/51')\nprint('train: 3/800')" + gate,
                          latents="print('caching 4/800')" + gate,
                          train="print('validation 6/800')" + gate + f"print({self.train_line!r})")
        for i, (phase, line) in enumerate(zip(self.phases, ("train: 3/800", "caching 4/800", "validation 6/800")), 1):
            st = self.wait_log(line)
            self.assertEqual((st["phase"], st["phase_index"], st["step"], st["total"]), (phase, i, 0, 800), st)
            (self.repo / (phase + ".go")).touch()
        st = self.wait_state({"done", "error"})
        self.assertEqual(st["state"], "done", st)
        # 跑完步數補滿（管線模式每 10 步才印一行，最後幾步可能沒印到）
        self.assertEqual((st["step"], st["total"], st["loss"], st["sec_per_step"], st["vram"]),
                         (800, 800, 0.2341, 5.75, "2.1/2.4/3.6G"))

    def test_step_lines_when_output_is_piped(self):
        # 2026-11-21 實機：接管線時 KohakuMoss 不畫進度條，只印 monitor 的 step 行和 VRAM peak 行
        gate = ("\nend = time.monotonic() + 10\n"
                "while not Path(__file__).with_suffix('.go').exists() and time.monotonic() < end:\n"
                "    time.sleep(0.03)\n")
        train = ("print('[21:52:46] [training.callbacks.kernels] [INFO] VRAM peak: prewarm 1.65 GiB allocated / 2.09 reserved; step 1 2.09 / 2.35', flush=True)\n"
                 "print('[21:52:46] [training.callbacks.monitor] [INFO] step 1 [loss=0.218, grad_norm=0.01126, lr=2e-06]', flush=True)\n"
                 "time.sleep(0.5)\n"
                 "print('[21:53:50] [training.callbacks.monitor] [INFO] step 10 [loss=0.4123, grad_norm=0.03857, lr=2e-05]', flush=True)"
                 + gate)
        self.start_kohaku(train=train)
        st = self.wait_log("[21:53:50] [training.callbacks.monitor] [INFO] step 10 [loss=0.4123, grad_norm=0.03857, lr=2e-05]")
        self.assertEqual((st["phase"], st["step"], st["total"], st["loss"], st["vram"]), ("train", 10, 800, 0.4123, "2.09/2.35G"), st)
        self.assertGreater(st["sec_per_step"], 0.03, st)
        self.assertLess(st["sec_per_step"], 1.0, st)
        (self.repo / "train.go").touch()
        st = self.wait_state({"done", "error"})
        self.assertEqual((st["state"], st["step"]), ("done", 800), st)

    def test_only_checkpoint_exports_are_loras(self):
        names = ["xk/checkpoints/step_00000200/export/xk-comfy.safetensors",
                 "xk/export/xk-comfy.safetensors", "xk/checkpoints/step_00000200/state.safetensors"]
        self.start_kohaku(train=f"for name in {names!r}:\n"
                          "    p = Path(os.environ['TRAIN_OUT'], name)\n"
                          "    p.parent.mkdir(parents=True, exist_ok=True)\n"
                          "    p.touch()")
        st = self.wait_state({"done", "error"})
        self.assertEqual(st["state"], "done", st)
        self.assertEqual([f["name"] for f in st["files"] if f["kind"] == "lora"], names[:1])

    def test_phase_failure_stops_pipeline(self):
        self.kohaku["train_config"] = "# STEPS = 800\n"
        self.start_kohaku(latents="sys.exit(3)", train="Path('started').touch()")
        st = self.wait_state({"done", "error"})
        self.assertEqual((st["state"], st["phase"], st["phase_index"], st["total"]), ("error", "latents", 2, 0), st)
        self.assertTrue(st["error"].startswith("第 2 段（整理圖片）失敗"), st)
        self.assertIn("3", st["error"])
        self.assertFalse((self.repo / "started").exists())

    def test_stop_kills_phase_tree_and_skips_train(self):
        child = ("import socket, time\nfrom pathlib import Path\n"
                 "s = socket.socket()\ns.bind(('127.0.0.1', 0))\ns.listen()\n"
                 "Path('port').write_text(str(s.getsockname()[1]))\nprint('child ready', flush=True)\ntime.sleep(15)")
        self.start_kohaku(latents=f"import subprocess\nsubprocess.run([sys.executable, '-I', '-c', {child!r}])",
                          train="Path('started').touch()")
        self.wait_log("child ready")
        req = {"engine": "kohakumoss", "kohaku": self.kohaku}
        self.assertEqual(self.call("POST", "/start", req)[0], 409)
        self.assertEqual(self.call("POST", "/stop")[0], 200)
        st = self.wait_state({"stopped"})
        self.assertEqual((st["phase"], st["phase_index"]), ("latents", 2))
        with self.assertRaises(ConnectionRefusedError):
            socket.create_connection(("127.0.0.1", int((self.repo / "port").read_text())), timeout=3).close()
        self.assertFalse((self.repo / "started").exists())

    def test_toolkit_engine_unchanged(self):
        idle = self.call("GET", "/status")[1]
        self.assertEqual((idle["engine"], idle["phase"], idle["phase_index"], idle["sec_per_step"], idle["vram"]),
                         ("toolkit", "", 0, None, None))
        self.start_kohaku(train=f"print({self.train_line!r})")
        self.wait_state({"done"})
        self.call("POST", "/dataset", self.zip_of({"0001.txt": b"t"}))
        for fields in ({}, {"engine": "toolkit"}):
            code, body = self.call("POST", "/start", dict(fields, config="steps: 100\n", name="lora"))
            self.assertEqual(code, 200, body)
            st = self.wait_state({"done", "error"})
            self.assertEqual((st["state"], st["step"], st["total"], st["loss"]), ("done", 10, 100, 0.12))
            self.assertEqual((st["engine"], st["phase"], st["phase_index"], st["sec_per_step"], st["vram"]),
                             ("toolkit", "", 0, None, None))


if __name__ == "__main__":
    unittest.main(verbosity=1)
