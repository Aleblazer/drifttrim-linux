import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami

Kirigami.ScrollablePage {
    id: page

    property var app
    property var categories: JSON.parse(app.categoriesKey)
    readonly property var apps: JSON.parse(app.appsKey)

    title: "Categories"

    function commit(next) {
        categories = next;
        app.send({ cmd: "setCategories", categories: next.map(c => ({ name: c.name, appKeys: c.appKeys })) });
    }

    function addCategory() {
        const name = newName.text.trim();
        if (name.length === 0 || categories.some(c => c.name.toLowerCase() === name.toLowerCase())) return;
        commit(categories.concat([{ name: name, appKeys: [] }]));
        newName.clear();
    }

    function toggle(categoryIndex, key, on) {
        const next = JSON.parse(JSON.stringify(categories));
        const keys = next[categoryIndex].appKeys.filter(k => k !== key);
        if (on) keys.push(key);
        next[categoryIndex].appKeys = keys;
        commit(next);
    }

    ColumnLayout {
        width: parent.width
        spacing: Kirigami.Units.largeSpacing

        Controls.Label {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            text: "A fader pointed at a category moves all its apps together. Apps in no category fall under Everything Else."
        }

        RowLayout {
            Layout.fillWidth: true
            Controls.TextField {
                id: newName
                Layout.fillWidth: true
                placeholderText: "New category name"
                maximumLength: 80
                onAccepted: page.addCategory()
            }
            Controls.Button { text: "Add"; icon.name: "list-add"; onClicked: page.addCategory() }
        }

        Repeater {
            model: page.categories
            delegate: Kirigami.AbstractCard {
                id: categoryCard
                required property var modelData
                required property int index
                Layout.fillWidth: true
                contentItem: ColumnLayout {
                    RowLayout {
                        Kirigami.Heading { Layout.fillWidth: true; level: 3; text: categoryCard.modelData.name }
                        Controls.ToolButton {
                            icon.name: "edit-delete"
                            Accessible.name: "Delete category"
                            onClicked: {
                                const next = page.categories.slice();
                                next.splice(categoryCard.index, 1);
                                page.commit(next);
                            }
                        }
                    }
                    Controls.Label {
                        visible: page.apps.length === 0
                        opacity: 0.7
                        text: "Play audio from an app and it will appear here."
                    }
                    Repeater {
                        model: page.apps
                        delegate: Controls.CheckBox {
                            required property var modelData
                            text: modelData.live ? modelData.name : modelData.name + " (not running)"
                            checked: categoryCard.modelData.appKeys.indexOf(modelData.key) >= 0
                            onToggled: page.toggle(categoryCard.index, modelData.key, checked)
                        }
                    }
                }
            }
        }
    }
}
