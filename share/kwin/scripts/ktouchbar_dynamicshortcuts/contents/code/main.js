// SPDX-License-Identifier: GPL-3.0-only
function resetInactivity() {
    callDBus('org.ktouchbar.DynamicShortcuts',
             '/org/ktouchbar/DynamicShortcuts',
             'org.ktouchbar.DynamicShortcuts',
             'ResetInactivity');
}

function setActiveWindow(window) {
    if (!window) {
        return;
    }
    callDBus('org.ktouchbar.DynamicShortcuts',
             '/org/ktouchbar/DynamicShortcuts',
             'org.ktouchbar.DynamicShortcuts',
             'SetActiveWindow',
             window.resourceClass, window.caption);
    resetInactivity();
}

if (workspace.windowActivated) {
    workspace.windowActivated.connect(setActiveWindow);
} else if (workspace.clientActivated) {
    workspace.clientActivated.connect(setActiveWindow);
}

// Also reset inactivity on desktop switch
if (workspace.currentDesktopChanged) {
    workspace.currentDesktopChanged.connect(resetInactivity);
}

// Reset inactivity when an activity is switched to
if (workspace.currentActivityChanged) {
    workspace.currentActivityChanged.connect(resetInactivity);
}
