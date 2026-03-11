from setuptools import setup, find_packages

setup(
    name="thd_tool",
    version="0.2",
    packages=find_packages(),
    install_requires=["sounddevice", "numpy", "scipy", "matplotlib"],
    entry_points={
        "console_scripts": [
            "ac = thd_tool.ac:main",
            "thd = thd_tool.cli:main",   # legacy
        ],
    },
)
