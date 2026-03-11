# parse.py
# Token-based CLI parser. Each token is classified by its unit suffix.
# Designed to translate cleanly to C++.
#
# Token types:
#   freq   -- 20hz, 1khz, 20000hz
#   level  -- 0dbu, -12dbfs, 775mvrms, 1vrms
#   time   -- 0.2s, 1s
#   step   -- 2db, 0.5db          (level step for level sweep)
#   ppd    -- 10ppd                (points per decade for freq sweep)
#
# Grammar:
#   ac sweep level   <start:level> <stop:level> <freq:freq> [<step:step>]
#   ac sweep freq    <start:freq>  <stop:freq>  <level:level> [<ppd:ppd>]
#   ac monitor thd   <level:level> <freq:freq>  [<interval:time>]
#   ac monitor spectrum <level:level> <freq:freq> [<interval:time>]
#   ac generate sine <level:level> [<freq:freq>]
#   ac calibrate     [<freq:freq>] [<level:level>]

import re
import sys
import math


# ---------------------------------------------------------------------------
# Token classifier
# ---------------------------------------------------------------------------

def _parse_level(s):
    """Return Vrms or (dbfs, float) tuple. Bare number = dBFS."""
    s = s.lower().strip()
    try:
        if s.endswith("dbu"):
            db = float(s[:-3])
            return ("dbu", db)   # converted at runtime using configured reference
        if s.endswith("dbfs"):
            return ("dbfs", float(s[:-4]))
        if s.endswith("mvrms") or s.endswith("mv"):
            return float(re.sub(r"mv(rms)?$", "", s)) / 1000.0
        if s.endswith("vrms") or s.endswith("v"):
            return float(re.sub(r"v(rms)?$", "", s))
        if s.endswith("mvpp") or s.endswith("vpp"):
            factor = 1000.0 if s.endswith("mvpp") else 1.0
            v = float(re.sub(r"m?vpp$", "", s)) / factor
            return v / (2.0 * math.sqrt(2.0))
        return ("dbfs", float(s))   # bare number = dBFS
    except ValueError:
        pass
    raise ValueError(f"not a level: {s!r}")


def _parse_freq(s):
    """Return Hz as float or raise ValueError. Bare number = Hz."""
    s = s.lower().strip()
    try:
        if s.endswith("khz"):
            return float(s[:-3]) * 1000.0
        if s.endswith("hz"):
            return float(s[:-2])
        return float(s)   # bare number = Hz
    except ValueError:
        pass
    raise ValueError(f"not a frequency: {s!r}")


def _parse_time(s):
    """Return seconds as float or raise ValueError."""
    s = s.lower().strip()
    try:
        if s.endswith("s"):
            return float(s[:-1])
    except ValueError:
        pass
    raise ValueError(f"not a time: {s!r}")


def _parse_step(s):
    """Return dB step as float or raise ValueError."""
    s = s.lower().strip()
    try:
        if s.endswith("db"):
            return float(s[:-2])
    except ValueError:
        pass
    raise ValueError(f"not a dB step: {s!r}")


def _parse_ppd(s):
    """Return points-per-decade as int or raise ValueError."""
    s = s.lower().strip()
    try:
        if s.endswith("ppd"):
            return int(float(s[:-3]))
    except ValueError:
        pass
    raise ValueError(f"not a ppd value: {s!r}")


def classify(token):
    """Return (type, value) for a token, or raise ValueError if unrecognised."""
    for kind, fn in [("ppd", _parse_ppd), ("step", _parse_step),
                     ("time", _parse_time), ("level", _parse_level),
                     ("freq", _parse_freq)]:
        try:
            return (kind, fn(token))
        except ValueError:
            pass
    raise ValueError(f"unrecognised token: {token!r}")


# ---------------------------------------------------------------------------
# Grammar matcher
# ---------------------------------------------------------------------------

class ParseError(Exception):
    pass


def _pull(tokens, kind, optional=False):
    """Pop and return the value of the next token if it matches kind."""
    for i, (k, v) in enumerate(tokens):
        if k == kind:
            tokens.pop(i)
            return v
        # stop looking past the first token of a different kind that isn't optional
    if optional:
        return None
    raise ParseError(f"expected a {kind} value")


def _classify_all(args):
    tokens = []
    for a in args:
        try:
            tokens.append(classify(a))
        except ValueError as e:
            raise ParseError(str(e))
    return tokens


# ---------------------------------------------------------------------------
# Subcommand parsers
# ---------------------------------------------------------------------------

ABBREVS = {
    # verbs
    "s": "sweep", "sw": "sweep",
    "m": "monitor", "mon": "monitor",
    "g": "generate", "gen": "generate",
    "c": "calibrate", "cal": "calibrate",
    # sweep nouns
    "l": "level", "lev": "level",
    "f": "frequency", "freq": "frequency",
    # monitor nouns
    "t": "thd",
    "sp": "spectrum", "spec": "spectrum",
    # generate nouns
    "si": "sine",
    # show plot after command
    "sh": "show",
    # calibrate show
    "ls": "list",
    # setup / devices
    "se": "setup", "set": "setup",
    "d": "devices", "dev": "devices", "devs": "devices",
    "o": "output", "out": "output",
    "i": "input",  "in":  "input",
}


def _expand(word):
    return ABBREVS.get(word.lower(), word.lower())


def _extract_show(args):
    """Remove 'show' / 'sh' from args list, return (cleaned_args, show_flag)."""
    show = False
    cleaned = []
    for a in args:
        if a.lower() in ("show", "sh"):
            show = True
        else:
            cleaned.append(a)
    return cleaned, show


def _parse_channels(token):
    """Parse a channel spec into a sorted list of 0-based ints.

    Examples:
        "11"      -> [11]
        "0,2,5"   -> [0, 2, 5]
        "0-11"    -> [0, 1, 2, ..., 11]
        "0-3,7"   -> [0, 1, 2, 3, 7]
    """
    channels = set()
    for part in token.split(","):
        part = part.strip()
        if "-" in part:
            lo, hi = part.split("-", 1)
            channels.update(range(int(lo), int(hi) + 1))
        else:
            channels.add(int(part))
    return sorted(channels)


def parse(argv):
    """
    Parse argv (sys.argv[1:]) and return a dict describing the command.
    Raises ParseError with a human-readable message on failure.
    """
    if not argv:
        raise ParseError("no command given")

    args = list(argv)
    verb = _expand(args.pop(0))

    # "ac calibrate show" / "ac cal show" -- check before _extract_show strips "show"
    if verb == "calibrate" and args and _expand(args[0]) in ("show", "list"):
        return {"cmd": "calibrate_show"}

    # strip optional trailing "show" keyword anywhere in args
    args, show_plot = _extract_show(args)

    if verb == "sweep":
        if not args:
            raise ParseError("sweep needs a noun: level | frequency")
        noun = _expand(args.pop(0))

        tokens = _classify_all(args)

        if noun == "level":
            # ac sweep level [<start:level> <stop:level> [<freq:freq>] [<step:step>]]
            start = _pull(tokens, "level", optional=True) or ("dbfs", -40.0)
            stop  = _pull(tokens, "level", optional=True) or ("dbfs",   0.0)
            freq  = _pull(tokens, "freq",  optional=True) or 1000.0
            step  = _pull(tokens, "step",  optional=True) or ("db", 2.0)
            if tokens:
                raise ParseError(f"unexpected token(s): {tokens}")
            return {"cmd": "sweep_level",
                    "start": start, "stop": stop,
                    "freq": freq, "step": step,
                    "show_plot": show_plot}

        elif noun == "frequency":
            # ac sweep frequency [<start:freq> <stop:freq>] [<level:level>] [<ppd:ppd>]
            start = _pull(tokens, "freq",  optional=True) or 20.0
            stop  = _pull(tokens, "freq",  optional=True) or 20000.0
            level = _pull(tokens, "level", optional=True) or ("dbfs", -12.0)
            ppd   = _pull(tokens, "ppd",   optional=True) or 10
            if tokens:
                raise ParseError(f"unexpected token(s): {tokens}")
            return {"cmd": "sweep_frequency",
                    "start": start, "stop": stop,
                    "level": level, "ppd": ppd,
                    "show_plot": show_plot}

        else:
            raise ParseError(f"unknown sweep noun: {noun!r}  (level | frequency)")

    elif verb == "monitor":
        if not args:
            raise ParseError("monitor needs a noun: thd | spectrum")
        noun   = _expand(args.pop(0))
        tokens   = _classify_all(args)
        level    = _pull(tokens, "level", optional=True) or ("dbu", 0.0)  # 0dBu -- converted at runtime
        freq     = _pull(tokens, "freq",  optional=True) or 1000.0
        interval = _pull(tokens, "time",  optional=True) or 1.0
        if tokens:
            raise ParseError(f"unexpected token(s): {tokens}")
        if noun == "thd":
            return {"cmd": "monitor_thd",
                    "level": level, "freq": freq,
                    "interval": interval,
                    "show_plot": show_plot}
        elif noun == "level":
            return {"cmd": "monitor_level",
                    "level": level, "freq": freq,
                    "interval": interval,
                    "show_plot": show_plot}
        elif noun == "spectrum":
            return {"cmd": "monitor_spectrum",
                    "level": level, "freq": freq,
                    "interval": interval,
                    "show_plot": show_plot}
        else:
            raise ParseError(f"unknown monitor noun: {noun!r}  (thd | level | spectrum)")

    elif verb == "generate":
        if not args:
            raise ParseError("generate needs a noun: sine")
        noun   = _expand(args.pop(0))
        if noun == "sine":
            # Check for a channel spec before classifying tokens:
            # looks like "11", "0-11", "0,2,5", "0-3,7" -- no suffix, contains digit
            channels = None
            if args and re.match(r'^[\d][\d,\-]*$', args[0]):
                channels = args.pop(0)
            tokens = _classify_all(args)
            level = _pull(tokens, "level", optional=True) or ("dbu", 0.0)
            freq  = _pull(tokens, "freq",  optional=True) or 1000.0
            if tokens:
                raise ParseError(f"unexpected token(s): {tokens}")
            return {"cmd": "generate_sine",
                    "level": level, "freq": freq,
                    "channels": channels,
                    "show_plot": show_plot}
        else:
            raise ParseError(f"unknown generate noun: {noun!r}  (sine)")

    elif verb == "calibrate":
        # Optional channel overrides: ac calibrate [output N] [input N] [freq] [level]
        result    = {"cmd": "calibrate", "show_plot": show_plot}
        remaining = list(args)
        clean     = []
        while remaining:
            key = _expand(remaining[0])
            if key in ("output", "input") and len(remaining) > 1:
                remaining.pop(0)
                val = remaining.pop(0)
                try:
                    result["output_channel" if key == "output" else "input_channel"] = int(val)
                except ValueError:
                    raise ParseError(f"calibrate: {key!r} value must be an integer, got {val!r}")
            else:
                clean.append(remaining.pop(0))
        tokens = _classify_all(clean)
        freq  = _pull(tokens, "freq",  optional=True)
        level = _pull(tokens, "level", optional=True)
        if tokens:
            raise ParseError(f"unexpected token(s): {tokens}")
        result["freq"]  = freq  or 1000.0
        result["level"] = level or ("dbfs", -10.0)
        return result

    elif verb == "devices":
        return {"cmd": "devices"}

    elif verb == "setup":
        # ac setup output <ch> input <ch> device <n>
        # tokens are keyword-value pairs, all optional
        result = {"cmd": "setup"}
        remaining = list(args)
        while remaining:
            key = _expand(remaining.pop(0))
            if not remaining:
                raise ParseError(f"setup: {key!r} needs a value")
            val = remaining.pop(0)
            if key in ("output", "input", "device"):
                try:
                    result[key] = int(val)
                except ValueError:
                    raise ParseError(f"setup: {key!r} value must be an integer, got {val!r}")
            elif key in ("dburef", "dbu"):
                try:
                    ref = _parse_level(val)
                    if isinstance(ref, tuple):
                        raise ValueError
                    result["dbu_ref_vrms"] = ref
                except ValueError:
                    raise ParseError(f"setup dburef: expected voltage e.g. 775mv or 0.775v, got {val!r}")
            else:
                raise ParseError(f"setup: unknown key {key!r}  (output | input | device | dburef)")
        return result

    else:
        raise ParseError(f"unknown command: {verb!r}  (sweep | monitor | generate | calibrate | setup | devices)")


# ---------------------------------------------------------------------------
# Usage
# ---------------------------------------------------------------------------

USAGE = """\
ac -- audio bench tool

  ac sweep level   <start> <stop> <freq> [<step>]
  ac sweep frequency <start> <stop> <level> [<ppd>]
  ac monitor thd   <level> <freq> [<interval>]
  ac monitor spectrum <level> <freq> [<interval>]
  ac generate sine <level> [<freq>]
  ac calibrate     [output N] [input N] [<freq>] [<level>]
  ac calibrate show

Units:
  frequency : 20hz  1khz  20000hz
  level     : 0dbu  -12dbfs  775mvrms  1vrms  2vpp
  step      : 2db   0.5db
  ppd       : 10ppd  (points per decade)
  interval  : 0.2s  1s

Abbreviations:
  sweep->s  monitor->m  generate->g  calibrate->c
  level->l  frequency->f  thd->t  spectrum->sp  sine->si

Notes:
  dBu and Vrms levels require prior calibration (ac calibrate).
  dBFS levels work without calibration.

Examples:
  ac devices
  ac setup output 11 input 0 device 0
  ac sweep level -20dbu 6dbu 1khz
  ac sweep level -40dbfs 0dbfs 1khz 2db
  ac sweep frequency 20hz 20khz 0dbu
  ac sweep frequency 20hz 20khz 0dbu 20ppd
  ac s f 20hz 20khz 0dbu
  ac monitor thd 0dbu 1khz
  ac monitor thd 0dbu 1khz 0.2s
  ac m t 0dbu 1khz 0.2s
  ac monitor spectrum 0dbu 1khz
  ac m sp -12dbfs 1khz
  ac generate sine 0dbu 1khz
  ac g si 0dbu
  ac calibrate show
  ac calibrate 1khz
  ac calibrate output 1 input 2 1khz
  ac cal out 1 in 2

  ac devices
  ac setup output 11 input 0 device 0
  ac setup output 1   # change just one value
"""


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(USAGE)
        sys.exit(0)
    try:
        result = parse(sys.argv[1:])
        import pprint
        pprint.pprint(result)
    except ParseError as e:
        print(f"error: {e}", file=sys.stderr)
        print(USAGE, file=sys.stderr)
        sys.exit(1)
