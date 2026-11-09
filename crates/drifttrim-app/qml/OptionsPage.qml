import QtQuick
import org.kde.kirigami as Kirigami
import org.kde.kirigamiaddons.formcard as FormCard

FormCard.FormCardPage {
    id: page

    property var app

    title: "Options"

    FormCard.FormHeader { title: "Behaviour" }
    FormCard.FormCard {
        FormCard.FormSwitchDelegate {
            text: "Soft takeover"
            description: "A physical fader takes control only after it reaches the current volume, so levels never jump."
            checked: page.app.appState.softTakeover
            onToggled: page.app.send({ cmd: "setSoftTakeover", enabled: checked })
        }
        FormCard.FormDelegateSeparator {}
        FormCard.FormComboBoxDelegate {
            text: "When the window is closed"
            textRole: "text"
            valueRole: "value"
            model: [
                { text: "Ask each time", value: "ask" },
                { text: "Keep running in tray", value: "tray" },
                { text: "Quit", value: "exit" }
            ]
            currentIndex: ["ask", "tray", "exit"].indexOf(page.app.appState.closeBehavior)
            onActivated: page.app.send({ cmd: "setCloseBehavior", behavior: currentValue })
        }
        FormCard.FormDelegateSeparator {}
        FormCard.FormSwitchDelegate {
            text: "Start at login"
            description: "Starts in the tray when you log in to Plasma."
            checked: page.app.appState.autostart
            onToggled: page.app.send({ cmd: "setAutostart", enabled: checked })
        }
    }

    FormCard.FormHeader { title: "Global shortcuts" }
    FormCard.FormCard {
        FormCard.FormButtonDelegate {
            text: "Open Plasma shortcut settings"
            description: "Find DriftTrim in the list to bind Volume Up, Volume Down and Mute for each virtual fader."
            icon.name: "preferences-desktop-keyboard-shortcut"
            onClicked: page.app.send({ cmd: "openShortcuts" })
        }
    }

    FormCard.FormHeader { title: "About" }
    FormCard.FormCard {
        FormCard.FormTextDelegate {
            text: "DriftTrim for Linux"
            description: "Version " + page.app.appState.version
        }
    }
}
