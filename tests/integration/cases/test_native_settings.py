"""Real DSH settings browser journeys for layout, theme, keyboard and diagnostics copy."""

import pytest

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_native_settings_advanced_fields_and_network_failure_copy(scenario):
    """Advanced port/CA fields are usable and an unreachable server shows bounded network copy."""
    scenario.browser(
        "settings-advanced",
        hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
    )


def test_native_settings_narrow_theme_and_keyboard(scenario, paired_device):
    """A 390px dark/light viewport keeps one column and keyboard navigation reaches menus and revoke confirmation."""
    scenario.browser(
        "settings-layout",
        hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
        deviceId=paired_device.identity["device_id"],
    )
