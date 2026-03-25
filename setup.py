from setuptools import setup, find_packages

setup(
    name="ac",
    version="0.3.0a1",
    packages=find_packages(),
    install_requires=["sounddevice", "numpy", "scipy", "matplotlib", "pyzmq", "pyserial"],
    extras_require={"dev": ["pytest"], "gui": ["pyqtgraph>=0.13"], "jack": ["jack-client"]},
    entry_points={
        "console_scripts": [
            "ac = ac.client.ac:main",
            "thd = ac.cli:main",   # legacy
            "ds = ds.cli:main",
        ],
    },
)
