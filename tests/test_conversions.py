"""Unit tests for conversions.py — pure math, no I/O."""
import math
import pytest
from thd_tool.conversions import (
    vrms_to_dbu, dbu_to_vrms, dbfs_to_vrms, fmt_vrms,
    set_dbu_ref, get_dbu_ref,
)


# Restore the default reference after tests that change it
@pytest.fixture(autouse=True)
def restore_dbu_ref():
    original = get_dbu_ref()
    yield
    set_dbu_ref(original)


def test_vrms_to_dbu_reference():
    # 0 dBu is defined as sqrt(0.001 * 600) = 0.77459667 Vrms
    set_dbu_ref(0.77459667)
    assert abs(vrms_to_dbu(0.77459667)) < 1e-6


def test_vrms_to_dbu_roundtrip():
    set_dbu_ref(0.77459667)
    for v in (0.001, 0.1, 0.5, 1.0, 5.0):
        assert abs(dbu_to_vrms(vrms_to_dbu(v)) - v) < v * 1e-9


def test_dbfs_to_vrms():
    # -20 dBFS with ref=1.0 Vrms should give 0.1 Vrms
    result = dbfs_to_vrms(-20.0, vrms_at_0dbfs=1.0)
    assert abs(result - 0.1) < 1e-9


def test_dbfs_to_vrms_unity():
    assert dbfs_to_vrms(0.0, vrms_at_0dbfs=0.5) == pytest.approx(0.5)


def test_fmt_vrms_millivolts():
    s = fmt_vrms(0.7746)
    assert "mVrms" in s
    assert "774" in s


def test_fmt_vrms_volts():
    s = fmt_vrms(1.5)
    assert "Vrms" in s
    assert "mVrms" not in s
