"""Shared pytest options for ALL hardware test suites.

Single registration point for --hil (Codex PR 86: previously registered
in three separate conftests — nucula, labgrid, nfc — causing ValueError
on combined-suite runs).
"""
import pytest


def pytest_addoption(parser):
    parser.addoption("--hil", action="store_true", default=False,
                     help="Run hardware-in-the-loop tests")


def pytest_configure(config):
    config.addinivalue_line("markers", "hil: hardware-in-the-loop test")
