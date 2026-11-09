import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Kirigami.Dialog {
    id: dialog

    property var app
    property string editing: ""
    readonly property var capture: app.appState.capture
    readonly property var sources: JSON.parse(app.sourcesKey)
    readonly property var selected: sources[deviceCombo.currentIndex] ?? null
    readonly property bool isMidi: selected ? selected.isMidi : false

    title: editing.length > 0 ? "Change fader source" : "Add physical fader"
    padding: Kirigami.Units.largeSpacing
    preferredWidth: Kirigami.Units.gridUnit * 28
    standardButtons: Kirigami.Dialog.NoButton
    customFooterActions: [
        Kirigami.Action {
            text: "Capture"
            icon.name: "dialog-ok-apply"
            enabled: dialog.capture !== null && dialog.capture.detected !== null
            onTriggered: {
                dialog.app.send({ cmd: "commitCapture" });
                dialog.close();
            }
        },
        Kirigami.Action {
            text: "Cancel"
            icon.name: "dialog-cancel"
            onTriggered: dialog.close()
        }
    ]

    function keyWithMode() {
        if (!selected) return "";
        if (!selected.isMidi) return selected.key;
        const parts = selected.key.split(":");
        parts[1] = profileCombo.currentValue;
        return parts.join(":");
    }

    function begin(faderId) {
        editing = faderId;
        deviceCombo.currentIndex = sources.length > 0 ? 0 : -1;
        profileCombo.currentIndex = 0;
        app.send({ cmd: "startCapture", key: keyWithMode(), editing: faderId.length > 0 ? faderId : null });
        open();
    }

    function reselect() {
        if (visible) app.send({ cmd: "setCaptureKey", key: keyWithMode() });
    }

    onClosed: app.send({ cmd: "cancelCapture" })

    ColumnLayout {
        spacing: Kirigami.Units.largeSpacing

        Kirigami.FormLayout {
            Layout.fillWidth: true
            Controls.ComboBox {
                id: deviceCombo
                Kirigami.FormData.label: "Device:"
                Layout.fillWidth: true
                textRole: "name"
                model: dialog.sources
                displayText: currentIndex < 0 ? "No devices found" : currentText
                onActivated: dialog.reselect()
            }
            Controls.ComboBox {
                id: profileCombo
                Kirigami.FormData.label: "Input profile:"
                visible: dialog.isMidi
                textRole: "text"
                valueRole: "value"
                model: [
                    { text: "14-bit CC", value: "cc14" },
                    { text: "7-bit CC", value: "cc7" },
                    { text: "14-bit CC (QMK/VIK heartbeat)", value: "qmk" }
                ]
                onActivated: dialog.reselect()
            }
        }

        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: true
            type: dialog.capture && dialog.capture.detected ? Kirigami.MessageType.Positive : Kirigami.MessageType.Information
            text: dialog.capture ? dialog.capture.message : "Connect a fader device to begin."
        }
    }
}
