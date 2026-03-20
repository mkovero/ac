from setuptools import setup, find_packages

setup(
    name="thd_tool",
    version="0.2",
    packages=find_packages(),
    install_requires=["sounddevice", "numpy", "scipy", "matplotlib", "pyzmq", "pyserial"],
    extras_require={"dev": ["pytest"], "gui": ["pyqtgraph>=0.13"], "jack": ["jack-client"]},
    entry_points={
        "console_scripts": [
            "ac = thd_tool.client.ac:main",
            "thd = thd_tool.cli:main",   # legacy
        ],
    },
)
