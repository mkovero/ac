"""Parser unit tests — no I/O, no JACK, pure Python."""
import pytest
from thd_tool.client.parse import parse, ParseError


# ---------------------------------------------------------------------------
# sweep level
# ---------------------------------------------------------------------------

def test_sweep_level_defaults():
    r = parse(["sweep", "level"])
    assert r["cmd"] == "sweep_level"
    assert r["start"] == ("dbfs", -40.0)
    assert r["stop"]  == ("dbfs",   0.0)
    assert r["freq"]  == 1000.0
    # default step: tuple form
    assert r["step"] == ("db", 2.0)
    assert r["show_plot"] is False


def test_sweep_level_dbu():
    r = parse(["sweep", "level", "-20dbu", "6dbu", "1khz"])
    assert r["cmd"]   == "sweep_level"
    assert r["start"] == ("dbu", -20.0)
    assert r["stop"]  == ("dbu",   6.0)
    assert r["freq"]  == 1000.0


def test_sweep_level_with_step():
    r = parse(["sweep", "level", "-40dbfs", "0dbfs", "1khz", "2db"])
    assert r["cmd"]   == "sweep_level"
    assert r["start"] == ("dbfs", -40.0)
    assert r["stop"]  == ("dbfs",   0.0)
    assert r["step"]  == 2.0


# ---------------------------------------------------------------------------
# sweep frequency
# ---------------------------------------------------------------------------

def test_sweep_frequency():
    r = parse(["sweep", "frequency", "20hz", "20khz", "0dbu", "20ppd"])
    assert r["cmd"]   == "sweep_frequency"
    assert r["start"] == 20.0
    assert r["stop"]  == 20000.0
    assert r["level"] == ("dbu", 0.0)
    assert r["ppd"]   == 20


def test_sweep_frequency_defaults():
    r = parse(["sweep", "frequency"])
    assert r["cmd"]   == "sweep_frequency"
    # start/stop default to None; client falls back to config range
    assert r["start"] is None
    assert r["stop"]  is None
    assert r["level"] == ("dbfs", -12.0)
    assert r["ppd"]   == 10


# ---------------------------------------------------------------------------
# monitor
# ---------------------------------------------------------------------------

def test_monitor_thd():
    r = parse(["monitor", "thd", "0dbu", "1khz", "0.5s"])
    assert r["cmd"]      == "monitor_thd"
    assert r["level"]    == ("dbu", 0.0)
    assert r["freq"]     == 1000.0
    assert r["interval"] == 0.5


def test_monitor_thd_default_level():
    # Default when no level given: -12 dBFS so it works without calibration
    r = parse(["monitor", "thd"])
    assert r["level"] == ("dbfs", -12.0)


def test_monitor_spectrum_abbreviations():
    r = parse(["m", "sp", "-12dbfs", "1khz"])
    assert r["cmd"]   == "monitor_spectrum"
    assert r["level"] == ("dbfs", -12.0)
    assert r["freq"]  == 1000.0
    assert r["interval"] == 0.1   # default for spectrum


# ---------------------------------------------------------------------------
# generate
# ---------------------------------------------------------------------------

def test_generate_sine():
    r = parse(["generate", "sine", "0dbu", "1khz"])
    assert r["cmd"]      == "generate_sine"
    assert r["level"]    == ("dbu", 0.0)
    assert r["freq"]     == 1000.0
    assert r["channels"] is None


def test_generate_sine_with_channels():
    r = parse(["g", "si", "0,2", "-12dbfs", "1khz"])
    assert r["cmd"]      == "generate_sine"
    assert r["channels"] == "0,2"
    assert r["level"]    == ("dbfs", -12.0)
    assert r["freq"]     == 1000.0


# ---------------------------------------------------------------------------
# calibrate
# ---------------------------------------------------------------------------

def test_calibrate():
    r = parse(["calibrate", "output", "1", "input", "2", "1khz", "-10dbfs"])
    assert r["cmd"]            == "calibrate"
    assert r["output_channel"] == 1
    assert r["input_channel"]  == 2
    assert r["freq"]           == 1000.0
    assert r["level"]          == ("dbfs", -10.0)


def test_calibrate_show():
    r = parse(["calibrate", "show"])
    assert r["cmd"] == "calibrate_show"


def test_calibrate_list():
    r = parse(["cal", "list"])
    assert r["cmd"] == "calibrate_show"


# ---------------------------------------------------------------------------
# server
# ---------------------------------------------------------------------------

def test_server_enable():
    r = parse(["server", "enable"])
    assert r["cmd"] == "server_enable"


def test_server_set_host():
    r = parse(["server", "192.168.1.5"])
    assert r["cmd"]  == "server_set_host"
    assert r["host"] == "192.168.1.5"


def test_server_default():
    r = parse(["server"])
    assert r["cmd"]  == "server_set_host"
    assert r["host"] == "localhost"


# ---------------------------------------------------------------------------
# abbreviations
# ---------------------------------------------------------------------------

def test_abbreviations_sweep_level():
    assert parse(["s", "l"])["cmd"] == "sweep_level"


def test_abbreviations_sweep_frequency():
    assert parse(["s", "f"])["cmd"] == "sweep_frequency"


def test_abbreviations_monitor_thd():
    assert parse(["m", "t"])["cmd"] == "monitor_thd"


def test_abbreviations_generate_sine():
    assert parse(["g", "si"])["cmd"] == "generate_sine"


def test_abbreviations_calibrate():
    assert parse(["c"])["cmd"] == "calibrate"


# ---------------------------------------------------------------------------
# show flag
# ---------------------------------------------------------------------------

def test_show_flag():
    r = parse(["s", "l", "show"])
    assert r["show_plot"] is True


def test_show_flag_abbreviation():
    r = parse(["sweep", "level", "sh"])
    assert r["show_plot"] is True


# ---------------------------------------------------------------------------
# error cases
# ---------------------------------------------------------------------------

def test_unknown_command():
    with pytest.raises(ParseError):
        parse(["boguscmd"])


def test_bad_token_in_sweep():
    with pytest.raises(ParseError):
        parse(["sweep", "level", "notaprice"])


def test_no_command():
    with pytest.raises(ParseError):
        parse([])
