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



def test_in_vrms():
    cal = Calibration()
    cal.vrms_at_0dbfs_in = 0.245
    # in_vrms = linear_rms * vrms_at_0dbfs_in
    assert cal.in_vrms(0.5) == pytest.approx(0.5 * 0.245, rel=1e-6)


def test_in_vrms_none_when_uncalibrated():
    cal = Calibration()
    assert cal.in_vrms(0.5) is None



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
