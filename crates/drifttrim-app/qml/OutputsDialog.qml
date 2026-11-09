import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Kirigami.Dialog {
    id: dialog

    property var app
    property string faderId: ""
    property var ranked: []
    readonly property var available: JSON.parse(app.outputsKey).filter(o => !ranked.some(r => r.id === o.id))

    title: "Output ranking"
    padding: Kirigami.Units.largeSpacing
    preferredWidth: Kirigami.Units.gridUnit * 30
    standardButtons: Kirigami.Dialog.Close

    function begin(id) {
        faderId = id;
        const fader = app.appState.faders.find(x => x.id === id);
        ranked = fader ? fader.rankedOutputs : [];
        open();
    }

    function commit(next) {
        ranked = next;
        app.send({ cmd: "setOutputs", id: faderId, outputs: next.map(o => o.id) });
    }

    function shift(index, delta) {
        const target = index + delta;
        if (target < 0 || target >= ranked.length) return;
        const next = ranked.slice();
        next.splice(target, 0, next.splice(index, 1)[0]);
        commit(next);
    }

    ColumnLayout {
        spacing: Kirigami.Units.largeSpacing

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            text: "The highest-ranked output that is connected is the one this fader drives. It fails over automatically when devices are plugged or unplugged."
        }

        Repeater {
            model: dialog.ranked
            delegate: RowLayout {
                required property var modelData
                required property int index
                Layout.fillWidth: true
                Controls.Label {
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                    text: (index + 1) + ". " + modelData.name
                }
                Controls.ToolButton { icon.name: "go-up"; Accessible.name: "Move up"; onClicked: dialog.shift(index, -1) }
                Controls.ToolButton { icon.name: "go-down"; Accessible.name: "Move down"; onClicked: dialog.shift(index, 1) }
                Controls.ToolButton {
                    icon.name: "list-remove"
                    Accessible.name: "Remove"
                    onClicked: {
                        const next = dialog.ranked.slice();
                        next.splice(index, 1);
                        dialog.commit(next);
                    }
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            Controls.ComboBox {
                id: addCombo
                Layout.fillWidth: true
                textRole: "name"
                model: dialog.available
                displayText: currentIndex < 0 ? "No more outputs" : currentText
            }
            Controls.Button {
                text: "Add"
                icon.name: "list-add"
                enabled: addCombo.currentIndex >= 0
                onClicked: dialog.commit(dialog.ranked.concat([dialog.available[addCombo.currentIndex]]))
            }
        }
    }
}
