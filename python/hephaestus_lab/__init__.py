"""Deterministic, read-only research helpers for Hephaestus evidence."""

from .statistics import (
    BootstrapInterval,
    ComparisonPolicy,
    FitnessVector,
    PairedOutcome,
    SelectionAnalysis,
    analyze_selection,
    histogram_bootstrap,
    paired_bootstrap,
    pareto_dominates,
)

__all__ = [
    "BootstrapInterval",
    "ComparisonPolicy",
    "FitnessVector",
    "PairedOutcome",
    "SelectionAnalysis",
    "analyze_selection",
    "histogram_bootstrap",
    "paired_bootstrap",
    "pareto_dominates",
]
