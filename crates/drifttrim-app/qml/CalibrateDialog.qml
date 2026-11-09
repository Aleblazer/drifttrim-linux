import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Kirigami.Dialog {
    id: dialog

    property var app
    property string faderId: ""
    readonly property var f: app.appState.faders.find(x => x.id === faderId) ?? null
    readonly property var record: app.appState.record
    readonly property bool recording: record !== null && !record.finished
    readonly property var tapers: ["linear", "audio", "straight", "linearReversed", "audioReversed"]

    title: f ? "Calibrate " + f.label : "Calibrate"
    padding: Kirigami.Units.largeSpacing
    preferredWidth: Kirigami.Units.gridUnit * 30
    standardButtons: Kirigami.Dialog.Close

    function begin(id) {
        faderId = id;
        open();
    }

    function apply() {
        app.send({
            cmd: "setCalibration", id: faderId, taper: tapers[taperCombo.currentIndex],
            inverted: invertBox.checked, muteRaw: muteSpin.value
        });
    }

    onClosed: if (record !== null) app.send({ cmd: "cancelRecord" })

    ColumnLayout {
        spacing: Kirigami.Units.largeSpacing

        Kirigami.FormLayout {
            Layout.fillWidth: true

            Controls.Label {
                Kirigami.FormData.label: "Live reading:"
                font.features: { "tnum": 1 }
                text: dialog.f && dialog.f.raw !== null ? dialog.f.raw + " " + dialog.f.units + " → " + dialog.f.value + "%" : "Waiting for the fader…"
            }
            Controls.Label {
                Kirigami.FormData.label: "Captured range:"
                text: dialog.f ? dialog.f.calMin + " – " + dialog.f.calMax + " " + dialog.f.units : ""
            }
            Controls.ComboBox {
                id: taperCombo
                Kirigami.FormData.label: "Taper:"
                model: ["Linear", "Audio", "Straight", "Linear (reversed wiring)", "Audio (reversed wiring)"]
                onActivated: dialog.apply()
                Binding on currentIndex {
                    value: dialog.f ? dialog.tapers.indexOf(dialog.f.taper) : 0
                    restoreMode: Binding.RestoreNone
                }
            }
            Controls.CheckBox {
                id: invertBox
                Kirigami.FormData.label: "Direction:"
                text: "Invert"
                onToggled: dialog.apply()
                Binding on checked {
                    value: dialog.f ? dialog.f.inverted : false
                    restoreMode: Binding.RestoreNone
                }
            }
            Controls.SpinBox {
                id: muteSpin
                Kirigami.FormData.label: "Mute threshold:"
                from: 0
                to: dialog.f ? dialog.f.rawMaximum : 3250
                editable: true
                onValueModified: dialog.apply()
                Binding on value {
                    value: dialog.f ? dialog.f.muteRaw : 0
                    restoreMode: Binding.RestoreNone
                }
            }
        }

        Kirigami.InlineMessage {
            Layout.fillWidth: true
            visible: dialog.record !== null
            type: dialog.record && dialog.record.finished
                ? (dialog.record.good ? Kirigami.MessageType.Positive : Kirigami.MessageType.Warning)
                : Kirigami.MessageType.Information
            text: !dialog.record ? "" : dialog.record.finished ? dialog.record.message
                : "Sweep the fader end to end, finishing at the bottom. " + dialog.record.samples + " samples so far. Volume is paused while recording."
        }

        RowLayout {
            Controls.Button {
                visible: !dialog.recording && !(dialog.record && dialog.record.finished)
                text: "Record range"
                icon.name: "media-record"
                onClicked: dialog.app.send({ cmd: "startRecord", id: dialog.faderId })
            }
            Controls.Button {
                visible: dialog.recording
                text: "Stop"
                icon.name: "media-playback-stop"
                onClicked: dialog.app.send({ cmd: "stopRecord" })
            }
            Controls.Button {
                visible: dialog.record !== null && dialog.record.finished
                text: "Use this range"
                icon.name: "dialog-ok-apply"
                onClicked: dialog.app.send({ cmd: "applyRecord" })
            }
            Controls.Button {
                visible: dialog.record !== null && dialog.record.finished
                text: "Discard"
                icon.name: "edit-delete"
                onClicked: dialog.app.send({ cmd: "cancelRecord" })
            }
        }
    }
}
