import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as Controls
import org.kde.kirigami as Kirigami
import org.drifttrim.app

Kirigami.ApplicationWindow {
    id: root

    title: "DriftTrim"
    width: Kirigami.Units.gridUnit * 50
    height: Kirigami.Units.gridUnit * 36
    minimumWidth: Kirigami.Units.gridUnit * 24
    minimumHeight: Kirigami.Units.gridUnit * 20
    visible: !backend.startHidden

    property bool quitting: false
    property var appState: ({
        faders: [], outputs: [], apps: [], categories: [], sources: [],
        capture: null, record: null, softTakeover: true,
        closeBehavior: "ask", audioReady: false, autostart: false, version: ""
    })
    property var faderIds: []

    // These strings change only when the lists do, so combo boxes built from
    // them stay open and stable while levels update many times a second.
    readonly property string outputsKey: JSON.stringify(appState.outputs.map(o => ({ id: o.id, name: o.name })))
    readonly property string appsKey: JSON.stringify(appState.apps)
    readonly property string categoriesKey: JSON.stringify(appState.categories)
    readonly property string sourcesKey: JSON.stringify(appState.sources)

    function send(message) { backend.command(JSON.stringify(message)); }

    function applyState() {
        if (backend.stateJson.length === 0) return;
        const next = JSON.parse(backend.stateJson);
        const ids = next.faders.map(f => f.id);
        if (JSON.stringify(ids) !== JSON.stringify(faderIds)) faderIds = ids;
        appState = next;
        if (next.screenshot && !shotTimer.running && !shotTimer.done) shotTimer.start();
    }

    function bringToFront() {
        root.show();
        root.raise();
        root.requestActivate();
    }

    function finishQuit() {
        quitting = true;
        Qt.quit();
    }

    function hideToTray() { root.hide(); }

    Backend {
        id: backend
        onStateJsonChanged: root.applyState()
        onShowRequested: root.bringToFront()
        onQuitRequested: root.finishQuit()
    }

    onClosing: close => {
        if (quitting) return;
        close.accepted = false;
        if (appState.closeBehavior === "tray") hideToTray();
        else if (appState.closeBehavior === "exit") finishQuit();
        else closeDialog.open();
    }

    Timer {
        id: shotTimer
        property bool done: false
        interval: 1500
        onTriggered: {
            done = true;
            root.pageStack.grabToImage(result => {
                result.saveToFile(root.appState.screenshot);
                root.finishQuit();
            });
        }
    }

    Kirigami.Dialog {
        id: closeDialog
        title: "Close DriftTrim?"
        padding: Kirigami.Units.largeSpacing
        standardButtons: Kirigami.Dialog.NoButton
        customFooterActions: [
            Kirigami.Action {
                text: "Keep running in tray"
                icon.name: "go-down-symbolic"
                onTriggered: {
                    if (rememberClose.checked) root.send({ cmd: "setCloseBehavior", behavior: "tray" });
                    closeDialog.close();
                    root.hideToTray();
                }
            },
            Kirigami.Action {
                text: "Quit"
                icon.name: "application-exit"
                onTriggered: {
                    if (rememberClose.checked) root.send({ cmd: "setCloseBehavior", behavior: "exit" });
                    closeDialog.close();
                    root.finishQuit();
                }
            }
        ]
        ColumnLayout {
            Controls.Label {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                text: "While it runs in the tray, your faders and shortcuts keep working."
            }
            Controls.CheckBox { id: rememberClose; text: "Don't ask again" }
        }
    }

    Kirigami.PromptDialog {
        id: removeDialog
        property string faderId: ""
        property string label: ""
        title: "Remove fader?"
        subtitle: "“" + label + "” will be removed from your layout."
        standardButtons: Kirigami.Dialog.Ok | Kirigami.Dialog.Cancel
        onAccepted: root.send({ cmd: "remove", id: faderId })
    }

    Kirigami.PromptDialog {
        id: renameDialog
        property string faderId: ""
        title: "Rename fader"
        standardButtons: Kirigami.Dialog.Ok | Kirigami.Dialog.Cancel
        onAccepted: root.send({ cmd: "rename", id: faderId, label: renameField.text })
        Controls.TextField { id: renameField; maximumLength: 80; onAccepted: renameDialog.accept() }
    }

    function askRemove(fader) {
        removeDialog.faderId = fader.id;
        removeDialog.label = fader.label;
        removeDialog.open();
    }

    function askRename(fader) {
        renameDialog.faderId = fader.id;
        renameField.text = fader.label;
        renameDialog.open();
        renameField.forceActiveFocus();
        renameField.selectAll();
    }

    CaptureDialog { id: captureDialog; app: root }
    CalibrateDialog { id: calibrateDialog; app: root }
    OutputsDialog { id: outputsDialog; app: root }

    Component { id: categoriesPage; CategoriesPage { app: root } }
    Component { id: optionsPage; OptionsPage { app: root } }

    function pushPage(component) {
        while (pageStack.depth > 1) pageStack.pop();
        pageStack.push(component);
    }

    pageStack.globalToolBar.style: Kirigami.ApplicationHeaderStyle.ToolBar
    pageStack.initialPage: Kirigami.ScrollablePage {
        id: mixerPage
        title: "Faders"

        actions: [
            Kirigami.Action {
                text: "Add fader"
                icon.name: "list-add"
                Kirigami.Action {
                    text: "Physical fader…"
                    icon.name: "input-keyboard"
                    onTriggered: captureDialog.begin("")
                }
                Kirigami.Action {
                    text: "Virtual fader"
                    icon.name: "view-media-equalizer"
                    onTriggered: root.send({ cmd: "addVirtual" })
                }
            },
            Kirigami.Action {
                text: "Categories"
                icon.name: "tag"
                onTriggered: root.pushPage(categoriesPage)
            },
            Kirigami.Action {
                text: "Options"
                icon.name: "configure"
                onTriggered: root.pushPage(optionsPage)
            }
        ]

        GridLayout {
            id: grid
            width: parent.width
            columns: width >= Kirigami.Units.gridUnit * 38 ? 2 : 1
            columnSpacing: Kirigami.Units.largeSpacing
            rowSpacing: Kirigami.Units.largeSpacing

            Repeater {
                model: root.faderIds
                delegate: FaderCard {
                    required property string modelData
                    faderId: modelData
                    app: root
                    calibrate: calibrateDialog
                    capture: captureDialog
                    outputs: outputsDialog
                    Layout.fillWidth: true
                    Layout.alignment: Qt.AlignTop
                    Layout.preferredWidth: 1
                }
            }
        }

        Kirigami.PlaceholderMessage {
            anchors.centerIn: parent
            width: parent.width - Kirigami.Units.gridUnit * 4
            visible: root.faderIds.length === 0
            icon.name: "view-media-equalizer"
            text: "No faders yet"
            explanation: "Add a physical fader from your keyboard or controller, or a virtual fader you can drag and bind to shortcuts."
            helpfulAction: Kirigami.Action {
                icon.name: "list-add"
                text: "Add physical fader…"
                onTriggered: captureDialog.begin("")
            }
        }

        footer: Controls.ToolBar {
            visible: !root.appState.audioReady
            contentItem: Controls.Label {
                text: "Connecting to the audio server…"
                horizontalAlignment: Text.AlignHCenter
            }
        }
    }
}
