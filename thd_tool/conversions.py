# conversions.py
import numpy as np
from .constants import DBU_REF_VRMS

def vrms_to_dbu(vrms):
    return 20.0 * np.log10(max(vrms, 1e-12) / DBU_REF_VRMS)

def dbu_to_vrms(dbu):
    return DBU_REF_VRMS * 10.0 ** (dbu / 20.0)

def dbfs_to_vrms(dbfs, vrms_at_0dbfs):
    return vrms_at_0dbfs * 10.0 ** (dbfs / 20.0)

def vrms_to_vpp(vrms):
    return vrms * 2.0 * np.sqrt(2.0)

def fmt_vrms(vrms):
    """Auto-scale to mVrms or Vrms."""
    if vrms < 1.0:
        return f"{vrms * 1000:.3f} mVrms"
    return f"{vrms:.4f} Vrms"

def fmt_vpp(vrms):
    vpp = vrms_to_vpp(vrms)
    if vpp < 1.0:
        return f"{vpp * 1000:.2f} mVpp"
    return f"{vpp:.4f} Vpp"

