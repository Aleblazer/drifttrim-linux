import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Kirigami.AbstractCard {
    id: card

    property string faderId: ""
    property var app
    property var calibrate
    property var capture
    property var outputs

    readonly property var f: app.appState.faders.find(x => x.id === faderId) ?? null
    readonly property string kind: f ? f.target : "output"

    // Rebuilt only when the list's text changes, so an open popup survives updates.
    readonly property string targetKey: {
        if (!f) return "[]";
        if (kind === "app") {
            const list = JSON.parse(app.appsKey).map(a => ({
                text: a.live ? a.name : a.name + " (not running)", value: a.key
            }));
            return JSON.stringify(list);
        }
        if (kind === "category") {
            const list = [{ text: "Everything Else", value: "#unassigned" }]
                .concat(JSON.parse(app.categoriesKey).map(c => ({ text: c.name, value: c.name })));
            return JSON.stringify(list);
        }
        const present = JSON.parse(app.outputsKey).map(o => ({ text: o.name, value: o.id }));
        const offline = JSON.stringify(f.rankedOutputs.filter(o => !present.some(p => p.value === o.id))
            .map(o => ({ text: o.name + " (offline)", value: o.id })));
        return JSON.stringify(present.concat(JSON.parse(offline)));
    }
    readonly property var targetModel: JSON.parse(targetKey)
    readonly property string selectedValue: {
        if (!f) return "";
        if (kind === "app") return f.appKey ?? "";
        if (kind === "category") return f.categoryName ?? "";
        return f.activeOutput ?? "";
    }

    function send(message) {
        message.id = faderId;
        app.send(message);
    }

    function chooseKind(next) {
        if (next === "output") {
            send({ cmd: "setTargetOutput", output: f.activeOutput ?? (JSON.parse(app.outputsKey)[0]?.id ?? null) });
        } else if (next === "app") {
            const apps = JSON.parse(app.appsKey);
            const choice = f.appKey ?? (apps[0]?.key ?? null);
            if (choice) send({ cmd: "setTargetApp", app: choice });
        } else {
            send({ cmd: "setTargetCategory", category: f.categoryName ?? "#unassigned" });
        }
    }

    function chooseTarget(value) {
        if (kind === "app") send({ cmd: "setTargetApp", app: value });
        else if (kind === "category") send({ cmd: "setTargetCategory", category: value });
        else send({ cmd: "setTargetOutput", output: value });
    }

    contentItem: ColumnLayout {
        spacing: Kirigami.Units.smallSpacing
        visible: card.f !== null

        RowLayout {
            Layout.fillWidth: true
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 0
                Kirigami.Heading {
                    Layout.fillWidth: true
                    level: 3
                    elide: Text.ElideRight
                    text: card.f ? card.f.label : ""
                }
                Controls.Label {
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                    font: Kirigami.Theme.smallFont
                    opacity: 0.7
                    text: card.f ? card.f.source : ""
                }
            }
            Controls.ToolButton {
                icon.name: card.f && card.f.muted ? "audio-volume-muted" : "audio-volume-high"
                checkable: true
                checked: card.f ? card.f.muted : false
                onClicked: card.send({ cmd: "toggleMute" })
                Controls.ToolTip.text: checked ? "Unmute this fader's target" : "Mute this fader's target"
                Controls.ToolTip.visible: hovered
                Controls.ToolTip.delay: Kirigami.Units.toolTipDelay
                Accessible.name: checked ? "Unmute" : "Mute"
            }
            Controls.ToolButton {
                icon.name: "overflow-menu"
                Accessible.name: "More actions"
                onClicked: menu.popup()
                Controls.Menu {
                    id: menu
                    Controls.MenuItem { text: "Rename…"; icon.name: "edit-rename"; onTriggered: card.app.askRename(card.f) }
                    Controls.MenuItem {
                        text: "Output ranking…"
                        icon.name: "view-sort"
                        onTriggered: card.outputs.begin(card.faderId)
                    }
                    Controls.MenuItem {
                        visible: card.f && !card.f.isVirtual
                        height: visible ? implicitHeight : 0
                        text: "Calibrate…"
                        icon.name: "configure"
                        onTriggered: card.calibrate.begin(card.faderId)
                    }
                    Controls.MenuItem {
                        visible: card.f && !card.f.isVirtual
                        height: visible ? implicitHeight : 0
                        text: "Change source…"
                        icon.name: "input-keyboard"
                        onTriggered: card.capture.begin(card.faderId)
                    }
                    Controls.MenuItem {
                        visible: card.f && card.f.isVirtual
                        height: visible ? implicitHeight : 0
                        text: "Reset to 100%"
                        icon.name: "edit-reset"
                        onTriggered: card.send({ cmd: "resetVirtual" })
                    }
                    Controls.MenuItem {
                        visible: card.f && card.f.isVirtual
                        height: visible ? implicitHeight : 0
                        text: "Set global shortcuts…"
                        icon.name: "preferences-desktop-keyboard-shortcut"
                        onTriggered: card.app.send({ cmd: "openShortcuts" })
                    }
                    Controls.MenuSeparator {}
                    Controls.MenuItem { text: "Move up"; icon.name: "go-up"; onTriggered: card.send({ cmd: "move", delta: -1 }) }
                    Controls.MenuItem { text: "Move down"; icon.name: "go-down"; onTriggered: card.send({ cmd: "move", delta: 1 }) }
                    Controls.MenuSeparator {}
                    Controls.MenuItem { text: "Remove"; icon.name: "edit-delete"; onTriggered: card.app.askRemove(card.f) }
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: Kirigami.Units.largeSpacing

            Item {
                Layout.fillWidth: true
                implicitHeight: slider.implicitHeight

                Controls.Slider {
                    id: slider
                    anchors.fill: parent
                    from: 0
                    to: 100
                    focusPolicy: card.f && card.f.isVirtual ? Qt.StrongFocus : Qt.NoFocus
                    opacity: card.f && card.f.live ? 1 : 0.6
                    Accessible.name: card.f ? card.f.label + " level" : ""
                    onMoved: card.send({ cmd: "setVirtual", value: Math.round(value), committed: false })
                    onPressedChanged: if (!pressed) card.send({ cmd: "setVirtual", value: Math.round(value), committed: true })
                    Binding on value {
                        when: !slider.pressed
                        value: card.f ? card.f.value : 0
                        restoreMode: Binding.RestoreNone
                    }
                }

                // A physical fader is display-only here: its position comes from
                // the hardware. This reads better than a disabled slider.
                MouseArea {
                    anchors.fill: parent
                    visible: card.f ? !card.f.isVirtual : false
                    acceptedButtons: Qt.AllButtons
                    preventStealing: true
                    onWheel: wheel => wheel.accepted = true
                }

                // Where the physical fader sits while it waits to take over.
                Rectangle {
                    visible: card.f !== null && card.f.pickupPosition !== null && card.f.pickupPosition !== undefined
                    width: Kirigami.Units.smallSpacing
                    height: parent.height * 0.7
                    radius: width / 2
                    anchors.verticalCenter: parent.verticalCenter
                    color: Kirigami.Theme.neutralTextColor
                    x: card.f && card.f.pickupPosition !== null
                        ? (parent.width - width) * card.f.pickupPosition / 100 : 0
                }
            }

            Controls.Label {
                Layout.minimumWidth: Kirigami.Units.gridUnit * 4
                horizontalAlignment: Text.AlignRight
                font.features: { "tnum": 1 }
                color: card.f && card.f.syncing ? Kirigami.Theme.neutralTextColor : Kirigami.Theme.textColor
                text: !card.f ? "" : card.f.syncing ? "Pickup " + card.f.pickupTarget + "%" : card.f.value + "%"
            }
        }

        RowLayout {
            Layout.fillWidth: true

            Controls.ComboBox {
                id: kindCombo
                textRole: "text"
                valueRole: "value"
                model: [
                    { text: "Output", value: "output" },
                    { text: "App", value: "app" },
                    { text: "Category", value: "category" }
                ]
                Accessible.name: "Target type"
                onActivated: card.chooseKind(currentValue)
                Binding on currentIndex {
                    value: ["output", "app", "category"].indexOf(card.kind)
                    restoreMode: Binding.RestoreNone
                }
            }

            Controls.ComboBox {
                id: targetCombo
                Layout.fillWidth: true
                Layout.minimumWidth: Kirigami.Units.gridUnit * 6
                textRole: "text"
                valueRole: "value"
                model: card.targetModel
                displayText: currentIndex < 0 ? "Choose…" : currentText
                Accessible.name: "Target"
                onActivated: card.chooseTarget(currentValue)
                Binding on currentIndex {
                    value: card.targetModel.findIndex(item => item.value === card.selectedValue)
                    restoreMode: Binding.RestoreNone
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            visible: card.f !== null

            Controls.Label {
                Layout.fillWidth: true
                elide: Text.ElideRight
                font: Kirigami.Theme.smallFont
                opacity: 0.7
                text: card.f ? card.f.status : ""
            }
            Controls.Label {
                visible: card.f ? !card.f.isVirtual : false
                font: Kirigami.Theme.smallFont
                text: "Max %"
            }
            Controls.SpinBox {
                visible: card.f ? !card.f.isVirtual : false
                from: 1
                to: 100
                editable: true
                value: card.f ? card.f.max : 100
                Accessible.name: "Maximum level percent"
                onValueModified: card.send({ cmd: "setMax", max: value })
            }
            Controls.Label {
                visible: card.f ? card.f.isVirtual : false
                font: Kirigami.Theme.smallFont
                text: "Step %"
            }
            Controls.SpinBox {
                visible: card.f ? card.f.isVirtual : false
                from: 1
                to: 100
                editable: true
                value: card.f ? card.f.step : 5
                Accessible.name: "Shortcut step percent"
                onValueModified: card.send({ cmd: "setStep", step: value })
            }
        }
    }
}
