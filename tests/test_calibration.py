"""Tests for the Calibration class — uses tmp_path for isolation."""
import math
import pytest
from ac.server.jack_calibration import Calibration
from ac.conversions import dbfs_to_vrms


# ---------------------------------------------------------------------------
# Key formatting
# ---------------------------------------------------------------------------

def test_cal_key():
    cal = Calibration(output_channel=2, input_channel=1)
    assert cal.key == "out2_in1"


# ---------------------------------------------------------------------------
# Save / load roundtrip
# ---------------------------------------------------------------------------

def test_save_load_roundtrip(tmp_path):
    path = str(tmp_path / "cal.json")
    cal = Calibration(output_channel=0, input_channel=0)
    cal.vrms_at_0dbfs_out = 0.245
    cal.vrms_at_0dbfs_in  = 0.240
    cal.save(path=path)

    loaded = Calibration.load(output_channel=0, input_channel=0, path=path)
    assert loaded is not None
    assert abs(loaded.vrms_at_0dbfs_out - 0.245) < 1e-9
    assert abs(loaded.vrms_at_0dbfs_in  - 0.240) < 1e-9
    assert loaded.key == cal.key


def test_load_missing(tmp_path):
    path = str(tmp_path / "no_such_file.json")
    assert Calibration.load(output_channel=0, input_channel=0, path=path) is None


def test_load_wrong_key(tmp_path):
    path = str(tmp_path / "cal.json")
    cal = Calibration(output_channel=0, input_channel=0)
    cal.vrms_at_0dbfs_out = 0.245
    cal.save(path=path)
    # Different channel pair → not found
    assert Calibration.load(output_channel=1, input_channel=0, path=path) is None


def test_load_all(tmp_path):
    path = str(tmp_path / "cal.json")
    for out_ch, in_ch in [(0, 0), (1, 0), (0, 1)]:
        cal = Calibration(output_channel=out_ch, input_channel=in_ch)
        cal.vrms_at_0dbfs_out = 0.245
        cal.save(path=path)

    all_cals = Calibration.load_all(path=path)
    assert len(all_cals) == 3


# ---------------------------------------------------------------------------
# response_curve save/load roundtrip
# ---------------------------------------------------------------------------

def test_response_curve_roundtrip(tmp_path):
    path = str(tmp_path / "cal.json")
    cal = Calibration(output_channel=0, input_channel=0)
    cal.vrms_at_0dbfs_out = 0.245
    cal.vrms_at_0dbfs_in  = 0.240
    cal.response_curve = [(100.0, -0.5), (1000.0, 0.0), (10000.0, 0.3)]
    cal.save(path=path)

    loaded = Calibration.load(output_channel=0, input_channel=0, path=path)
    assert loaded.response_curve is not None
    assert len(loaded.response_curve) == 3
    assert abs(loaded.response_curve[0][1] - (-0.5)) < 1e-9
    assert abs(loaded.response_curve[1][1] - 0.0) < 1e-9
    assert abs(loaded.response_curve[2][1] - 0.3) < 1e-9


# ---------------------------------------------------------------------------
# output_ok / input_ok properties
# ---------------------------------------------------------------------------

def test_output_ok_false_by_default():
    cal = Calibration()
    assert cal.output_ok is False


def test_input_ok_false_by_default():
    cal = Calibration()
    assert cal.input_ok is False


def test_output_ok_after_setting():
    cal = Calibration()
    cal.vrms_at_0dbfs_out = 0.245
    assert cal.output_ok is True


# ---------------------------------------------------------------------------
# response_db interpolation
# ---------------------------------------------------------------------------

def test_response_db_no_curve():
    cal = Calibration()
    assert cal.response_db(1000.0) == 0.0


def test_response_db_exact_point():
    cal = Calibration()
    cal.response_curve = [(100.0, -0.5), (1000.0, 0.0), (10000.0, 0.3)]
    assert abs(cal.response_db(100.0) - (-0.5)) < 1e-9
    assert abs(cal.response_db(1000.0) - 0.0) < 1e-9
    assert abs(cal.response_db(10000.0) - 0.3) < 1e-9


def test_response_db_interpolation():
    cal = Calibration()
    # linear in log-freq from 100 Hz (-0.5 dB) to 10000 Hz (0.5 dB)
    cal.response_curve = [(100.0, -0.5), (10000.0, 0.5)]
    # at geometric mean (1000 Hz) should be midpoint: 0.0 dB
    result = cal.response_db(1000.0)
    assert abs(result - 0.0) < 1e-6


def test_response_db_clamp_low():
    cal = Calibration()
    cal.response_curve = [(100.0, -0.5), (1000.0, 0.0)]
    # below range → clamp to first value
    assert abs(cal.response_db(10.0) - (-0.5)) < 1e-9


def test_response_db_clamp_high():
    cal = Calibration()
    cal.response_curve = [(100.0, -0.5), (1000.0, 0.0)]
    # above range → clamp to last value
    assert abs(cal.response_db(10000.0) - 0.0) < 1e-9


# ---------------------------------------------------------------------------
# out_vrms / in_vrms helpers
# ---------------------------------------------------------------------------

def test_out_vrms(tmp_path):
    cal = Calibration()
    cal.vrms_at_0dbfs_out = 0.245
    result = cal.out_vrms(-20.0)
    # dbfs_to_vrms(-20, 0.245) = 0.245 * 10^(-20/20) = 0.245 * 0.1
    assert result == pytest.approx(0.1 * 0.245, rel=1e-6)


def test_out_vrms_none_when_uncalibrated():
    cal = Calibration()
    assert cal.out_vrms(-20.0) is None


def test_out_vrms_with_response():
    cal = Calibration()
    cal.vrms_at_0dbfs_out = 0.245
    # response_db at 50 Hz = -0.3 dB (exact point in curve)
    cal.response_curve = [(50.0, -0.3), (1000.0, 0.0)]
    # out_vrms(0.0, 50Hz) = dbfs_to_vrms(0.0 - (-0.3), 0.245) = dbfs_to_vrms(0.3, 0.245)
    expected = dbfs_to_vrms(0.3, 0.245)
    result = cal.out_vrms(0.0, freq_hz=50.0)
    assert result == pytest.approx(expected, rel=1e-6)


def test_in_vrms():
    cal = Calibration()
    cal.vrms_at_0dbfs_in = 0.245
    # in_vrms = linear_rms * vrms_at_0dbfs_in
    assert cal.in_vrms(0.5) == pytest.approx(0.5 * 0.245, rel=1e-6)


def test_in_vrms_none_when_uncalibrated():
    cal = Calibration()
    assert cal.in_vrms(0.5) is None


def test_in_vrms_with_response():
    cal = Calibration()
    cal.vrms_at_0dbfs_in = 0.245
    # response_db at 50 Hz = -0.3 dB (exact point) → in_vrms = rms * v_in / 10^(-0.3/20)
    cal.response_curve = [(50.0, -0.3), (1000.0, 0.0)]
    linear_rms = 0.5
    delta = -0.3
    expected = linear_rms * 0.245 / (10.0 ** (delta / 20.0))
    result = cal.in_vrms(linear_rms, freq_hz=50.0)
    assert result == pytest.approx(expected, rel=1e-6)


# ---------------------------------------------------------------------------
# load_output_only
# ---------------------------------------------------------------------------

def test_load_output_only(tmp_path):
    path = str(tmp_path / "cal.json")
    # Save with in_ch=5 (unusual channel)
    cal = Calibration(output_channel=0, input_channel=5)
    cal.vrms_at_0dbfs_out = 0.245
    cal.save(path=path)

    loaded = Calibration.load_output_only(output_channel=0, path=path)
    assert loaded is not None
    assert abs(loaded.vrms_at_0dbfs_out - 0.245) < 1e-9


def test_load_output_only_missing(tmp_path):
    path = str(tmp_path / "cal.json")
    assert Calibration.load_output_only(output_channel=0, path=path) is None
