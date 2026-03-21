# gpio.py — GPIO handler for USB2GPIO (Mega2560)
#
# USB2GPIO protocol (binary serial, 115200 8N1):
#   MCU→Host: [0xAA][pin][value][ts_lo][ts_hi]  (5 bytes)
#     value=0 = button pressed (active-low INPUT_PULLUP)
#   Host→MCU: [0x55][cmd][pin][value]           (4 bytes)
#     cmd=1 → SET_OUTPUT (value: 0=LOW, 1=HIGH)
#     cmd=2 → SET_MODE   (value: 0=INPUT w/ pullup, 1=OUTPUT)
import json
import math
import queue
import threading

INPUT_PINS  = [2, 3, 20, 21]
OUTPUT_PINS = [13, 14, 15]

PIN_STOP     = 2
PIN_GEN_SINE = 3
PIN_GEN_PINK = 20
PIN_LED_SINE = 11
PIN_LED_PINK = 10
PIN_LED_BUSY = 13

CTRL_PORT = 5556
DATA_PORT = 5557

CMD_SET_OUTPUT = 1
CMD_SET_MODE   = 2
MODE_INPUT     = 0   # INPUT w/ pullup
MODE_OUTPUT    = 1


class GpioHandler:
    def __init__(self, serial_port, zmq_host="localhost", log_fn=None):
        import serial
        import zmq
        self._serial_port = serial_port
        self._zmq_host    = zmq_host
        self._log_fn      = log_fn
        self._ser         = serial.Serial(serial_port, 115200, timeout=0.1, write_timeout=0.1)
        self._serial_lock = threading.Lock()
        self._queue       = queue.Queue()
        self._stop_ev     = threading.Event()

        self._zmq_ctx = zmq.Context()
        self._req     = self._zmq_ctx.socket(zmq.REQ)
        self._req.setsockopt(zmq.LINGER,   0)
        self._req.setsockopt(zmq.RCVTIMEO, 2000)
        self._req.connect(f"tcp://{zmq_host}:{CTRL_PORT}")

        self._sub = self._zmq_ctx.socket(zmq.SUB)
        self._sub.setsockopt(zmq.SUBSCRIBE, b"")
        self._sub.setsockopt(zmq.LINGER,    0)
        self._sub.connect(f"tcp://{zmq_host}:{DATA_PORT}")

        self._sine_active  = False
        self._pink_active  = False
        self._level_dbfs   = -20.0
        self._out_channel  = 0
        self._req_lock     = threading.Lock()
        self._serial_dead  = False

    def _log(self, msg):
        if self._log_fn:
            self._log_fn(f"[GPIO] {msg}")

    def _send_zmq(self, cmd_dict):
        """Send ZMQ REQ command, return reply dict or None."""
        import zmq
        with self._req_lock:
            try:
                self._req.send_json(cmd_dict)
                return self._req.recv_json()
            except zmq.Again:
                # REQ socket is broken after a recv timeout — recreate it
                self._req.close()
                self._req = self._zmq_ctx.socket(zmq.REQ)
                self._req.setsockopt(zmq.LINGER,   0)
                self._req.setsockopt(zmq.RCVTIMEO, 2000)
                self._req.connect(f"tcp://{self._zmq_host}:{CTRL_PORT}")
                return None
            except zmq.ZMQError:
                return None

    def _serial_write(self, data):
        """Write bytes to serial under lock. Sets _serial_dead and logs on first error."""
        with self._serial_lock:
            try:
                self._ser.write(data)
            except Exception as e:
                if not self._serial_dead:
                    self._serial_dead = True
                    self._log(f"serial write error: {e}")

    def set_output(self, pin, value):
        self._serial_write(bytes([0x55, CMD_SET_OUTPUT, pin, value]))

    def set_mode(self, pin, mode):
        self._serial_write(bytes([0x55, CMD_SET_MODE, pin, mode]))

    def _update_leds(self):
        self.set_output(PIN_LED_SINE, 1 if self._sine_active else 0)
        self.set_output(PIN_LED_PINK, 1 if self._pink_active else 0)
        self.set_output(PIN_LED_BUSY, 1 if (self._sine_active or self._pink_active) else 0)

    @property
    def status(self):
        return {
            "port":         self._serial_port,
            "channel":      self._out_channel,
            "level_dbfs":   self._level_dbfs,
            "sine_active":  self._sine_active,
            "pink_active":  self._pink_active,
            "serial_dead":  self._serial_dead,
        }

    def _resolve_level_dbfs(self):
        """Fetch current calibration and return 0 dBu in dBFS, or -20 dBFS if uncalibrated."""
        cal = self._send_zmq({"cmd": "get_calibration"})
        if cal and cal.get("vrms_at_0dbfs_out"):
            vrms_ref = 0.7745966692  # 0 dBu
            dbfs = 20.0 * math.log10(vrms_ref / cal["vrms_at_0dbfs_out"])
            level = max(-60.0, min(-0.5, dbfs))
            self._log(f"calibration: vrms_at_0dbfs_out={cal['vrms_at_0dbfs_out']:.4f}  ->  {level:.2f} dBFS")
        else:
            level = -20.0
            self._log("calibration unavailable, using fallback -20.00 dBFS")
        self._level_dbfs = level
        return level

    def start(self):
        """Configure pins, fetch calibration, start threads. Returns immediately."""
        for pin in INPUT_PINS:
            self.set_mode(pin, MODE_INPUT)
        for pin in OUTPUT_PINS:
            self.set_mode(pin, MODE_OUTPUT)
        for pin in OUTPUT_PINS:
            self.set_output(pin, 0)

        self._log(f"serial port opened: {self._serial_port}")

        # Initial level resolution (cached in _level_dbfs for status display)
        self._resolve_level_dbfs()

        # Read output channel from server config
        ack = self._send_zmq({"cmd": "setup", "update": {}})
        if ack and ack.get("config"):
            self._out_channel = ack["config"].get("output_channel", 0)
        self._log(f"output channel: {self._out_channel}")

        threading.Thread(target=self._serial_reader_thread,   daemon=True).start()
        threading.Thread(target=self._event_processor_thread, daemon=True).start()
        threading.Thread(target=self._zmq_sub_thread,         daemon=True).start()
        self._log("threads started")

    def stop(self):
        """Signal threads to exit and release resources."""
        self._log("stopping")
        self._stop_ev.set()
        for pin in OUTPUT_PINS:
            try:
                self.set_output(pin, 0)
            except Exception:
                pass
        for obj in (self._ser, self._req, self._sub):
            try:
                obj.close()
            except Exception:
                pass
        try:
            self._zmq_ctx.term()
        except Exception:
            pass

    # ------------------------------------------------------------------
    # Thread: serial reader
    # ------------------------------------------------------------------

    def _serial_reader_thread(self):
        buf = bytearray()
        while not self._stop_ev.is_set():
            try:
                data = self._ser.read(64)
            except Exception as e:
                if not self._serial_dead:
                    self._serial_dead = True
                    self._log(f"serial read error: {e}  — stopping")
                break
            if not data:
                continue
            buf.extend(data)
            while len(buf) >= 5:
                start = buf.find(0xAA)
                if start == -1:
                    self._log("serial sync lost, buffer cleared")
                    buf.clear()
                    break
                if start > 0:
                    del buf[:start]
                if len(buf) < 5:
                    break
                # [0xAA][pin][value][ts_lo][ts_hi]
                _, pin, value, _ts_lo, _ts_hi = buf[:5]
                del buf[:5]
                self._queue.put((pin, value))

    # ------------------------------------------------------------------
    # Thread: event processor
    # ------------------------------------------------------------------

    def _event_processor_thread(self):
        while not self._stop_ev.is_set():
            try:
                pin, value = self._queue.get(timeout=0.1)
            except queue.Empty:
                continue

            if value != 0:
                continue  # only react to press (active-low)

            if pin == PIN_STOP:
                self._log("button: STOP")
                self._send_zmq({"cmd": "stop"})
                self._sine_active = False
                self._pink_active = False
                self._update_leds()

            elif pin == PIN_GEN_SINE:
                if not self._sine_active:
                    if self._pink_active:
                        self._log("stopping pink before starting sine")
                        self._send_zmq({"cmd": "stop", "name": "generate_pink"})
                        self._pink_active = False
                    level = self._resolve_level_dbfs()
                    self._log(f"button: SINE -> generate 1 kHz @ {level:.2f} dBFS ch {self._out_channel}")
                    self._sine_active = True
                    self._update_leds()   # optimistic
                    ack = self._send_zmq({
                        "cmd":        "generate",
                        "freq_hz":    1000.0,
                        "level_dbfs": level,
                        "channels":   [self._out_channel],
                    })
                    if ack and ack.get("ok"):
                        self._log("-> ok")
                    else:
                        self._log(f"-> failed: {ack}")
                        self._sine_active = False
                        self._update_leds()

            elif pin == PIN_GEN_PINK:
                if not self._pink_active:
                    if self._sine_active:
                        self._log("stopping sine before starting pink")
                        self._send_zmq({"cmd": "stop", "name": "generate"})
                        self._sine_active = False
                    level = self._resolve_level_dbfs()
                    self._log(f"button: PINK -> generate_pink @ {level:.2f} dBFS ch {self._out_channel}")
                    self._pink_active = True
                    self._update_leds()   # optimistic
                    ack = self._send_zmq({
                        "cmd":        "generate_pink",
                        "level_dbfs": level,
                        "channels":   [self._out_channel],
                    })
                    if ack and ack.get("ok"):
                        self._log("-> ok")
                    else:
                        self._log(f"-> failed: {ack}")
                        self._pink_active = False
                        self._update_leds()
            # pin 21 → reserved, ignored

    # ------------------------------------------------------------------
    # Thread: ZMQ SUB — watch for done/error frames to update LEDs
    # ------------------------------------------------------------------

    def _zmq_sub_thread(self):
        import zmq
        poller = zmq.Poller()
        poller.register(self._sub, zmq.POLLIN)
        while not self._stop_ev.is_set():
            try:
                socks = dict(poller.poll(200))
            except zmq.ZMQError:
                break
            if self._sub not in socks:
                continue
            try:
                msg = self._sub.recv(zmq.NOBLOCK)
            except zmq.Again:
                continue
            except zmq.ZMQError:
                break
            space = msg.find(b" ")
            if space == -1:
                continue
            topic = msg[:space].decode("utf-8", errors="replace")
            try:
                frame = json.loads(msg[space + 1:])
            except Exception:
                continue
            if topic in ("done", "error"):
                cmd_name = frame.get("cmd")
                self._log(f"pub event: {topic}  cmd={cmd_name}")
                if cmd_name == "generate":
                    self._sine_active = False
                elif cmd_name == "generate_pink":
                    self._pink_active = False
                self._update_leds()
