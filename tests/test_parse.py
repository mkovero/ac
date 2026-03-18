"""Parser unit tests — no I/O, no JACK, pure Python."""
import pytest
from thd_tool.client.parse import parse, ParseError


# ---------------------------------------------------------------------------
# sweep level
# ---------------------------------------------------------------------------

def test_sweep_level_defaults():
    r = parse(["sweep", "level"])
    assert r["cmd"]      == "sweep_level"
    assert r["start"]    == ("dbfs", -40.0)
    assert r["stop"]     == ("dbfs",   0.0)
    assert r["freq"]     == 1000.0
    assert r["duration"] == 1.0
    assert r["show_plot"] is False


def test_sweep_level_dbu():
    r = parse(["sweep", "level", "-20dbu", "6dbu", "1khz"])
    assert r["cmd"]   == "sweep_level"
    assert r["start"] == ("dbu", -20.0)
    assert r["stop"]  == ("dbu",   6.0)
    assert r["freq"]  == 1000.0


def test_sweep_level_with_duration():
    r = parse(["sweep", "level", "-40dbfs", "0dbfs", "1khz", "2s"])
    assert r["cmd"]      == "sweep_level"
    assert r["start"]    == ("dbfs", -40.0)
    assert r["stop"]     == ("dbfs",   0.0)
    assert r["duration"] == 2.0


# ---------------------------------------------------------------------------
# sweep frequency
# ---------------------------------------------------------------------------

def test_sweep_frequency():
    r = parse(["sweep", "frequency", "20hz", "20khz", "0dbu"])
    assert r["cmd"]      == "sweep_frequency"
    assert r["start"]    == 20.0
    assert r["stop"]     == 20000.0
    assert r["level"]    == ("dbu", 0.0)
    assert r["duration"] == 1.0


def test_sweep_frequency_with_duration():
    r = parse(["sweep", "frequency", "20hz", "20khz", "0dbu", "5s"])
    assert r["cmd"]      == "sweep_frequency"
    assert r["duration"] == 5.0


def test_sweep_frequency_defaults():
    r = parse(["sweep", "frequency"])
    assert r["cmd"]      == "sweep_frequency"
    # start/stop default to None; client falls back to config range
    assert r["start"]    is None
    assert r["stop"]     is None
    assert r["level"]    == ("dbfs", -20.0)
    assert r["duration"] == 1.0


# ---------------------------------------------------------------------------
# monitor (unified)
# ---------------------------------------------------------------------------

def test_monitor_defaults():
    r = parse(["monitor"])
    assert r["cmd"]        == "monitor"
    assert r["start_freq"] == 20.0
    assert r["end_freq"]   == 20000.0
    assert r["interval"]   == 0.1
    assert r["min_y"]      is None
    assert r["max_y"]      is None


def test_monitor_with_freq():
    r = parse(["monitor", "1khz"])
    assert r["cmd"]        == "monitor"
    assert r["start_freq"] == 1000.0
    assert r["end_freq"]   == 20000.0


def test_monitor_with_range():
    r = parse(["monitor", "100hz", "10khz"])
    assert r["cmd"]        == "monitor"
    assert r["start_freq"] == 100.0
    assert r["end_freq"]   == 10000.0


def test_monitor_with_interval():
    r = parse(["monitor", "1khz", "0.2s"])
    assert r["cmd"]        == "monitor"
    assert r["start_freq"] == 1000.0
    assert r["interval"]   == 0.2


def test_monitor_backward_compat_thd():
    """Old 'ac monitor thd' noun is accepted and stripped; level→min_y, freq→start_freq."""
    r = parse(["monitor", "thd", "0dbu", "1khz", "0.5s"])
    assert r["cmd"]        == "monitor"
    assert r["start_freq"] == 1000.0
    assert r["min_y"]      == ("dbu", 0.0)
    assert r["interval"]   == 0.5


def test_monitor_backward_compat_spectrum():
    r = parse(["m", "sp", "-12dbfs", "1khz"])
    assert r["cmd"]        == "monitor"
    assert r["start_freq"] == 1000.0
    assert r["min_y"]      == ("dbfs", -12.0)


def test_monitor_abbreviations():
    assert parse(["m"])["cmd"]     == "monitor"
    assert parse(["m", "t"])["cmd"] == "monitor"
    assert parse(["m", "sp"])["cmd"] == "monitor"


# ---------------------------------------------------------------------------
# plot
# ---------------------------------------------------------------------------

def test_plot_defaults():
    r = parse(["plot"])
    assert r["cmd"]   == "plot"
    assert r["start"] is None
    assert r["stop"]  is None
    assert r["level"] == ("dbfs", -20.0)
    assert r["ppd"]   == 10


def test_plot_full():
    r = parse(["plot", "20hz", "20khz", "0dbu", "20ppd"])
    assert r["cmd"]   == "plot"
    assert r["start"] == 20.0
    assert r["stop"]  == 20000.0
    assert r["level"] == ("dbu", 0.0)
    assert r["ppd"]   == 20


def test_plot_abbreviations():
    assert parse(["p"])["cmd"]  == "plot"
    assert parse(["pl"])["cmd"] == "plot"


# ---------------------------------------------------------------------------
# generate
# ---------------------------------------------------------------------------

def test_generate_sine():
    r = parse(["generate", "sine", "0dbu", "1khz"])
    assert r["cmd"]      == "generate_sine"
    assert r["level"]    == ("dbu", 0.0)
    assert r["freq"]     == 1000.0
    assert r["channels"] is None


def test_generate_sine_default_level():
    r = parse(["generate", "sine"])
    assert r["cmd"]   == "generate_sine"
    assert r["level"] is None   # resolved at runtime: 0dBu if calibrated, else -20dBFS


def test_generate_pink_default_level():
    r = parse(["generate", "pink"])
    assert r["cmd"]   == "generate_pink"
    assert r["level"] is None


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


def test_sweep_level_old_step_token_rejected():
    """Old 'ac sweep level ... 2db' step syntax now raises ParseError."""
    with pytest.raises(ParseError):
        parse(["sweep", "level", "-40dbfs", "0dbfs", "1khz", "2db"])


def test_sweep_frequency_old_ppd_token_rejected():
    """Old 'ac sweep frequency ... 20ppd' ppd syntax now raises ParseError."""
    with pytest.raises(ParseError):
        parse(["sweep", "frequency", "20hz", "20khz", "0dbu", "20ppd"])
