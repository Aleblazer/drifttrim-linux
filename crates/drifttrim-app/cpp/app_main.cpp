#include "app_main.h"

#include <QApplication>
#include <QByteArray>
#include <QIcon>
#include <QQmlApplicationEngine>
#include <QQuickStyle>
#include <QUrl>

// A QApplication rather than the QGuiApplication CXX-Qt wraps: the Plasma
// control style draws through QStyle, which needs QtWidgets to be initialised.
int drifttrim_run_app()
{
    static int argc = 1;
    static char name[] = "drifttrim";
    static char *argv[] = {name, nullptr};

    QApplication app(argc, argv);
    QApplication::setOrganizationDomain(QStringLiteral("drifttrim.org"));
    QApplication::setApplicationName(QStringLiteral("drifttrim"));
    QApplication::setApplicationDisplayName(QStringLiteral("DriftTrim"));
    QApplication::setDesktopFileName(QStringLiteral("org.drifttrim.DriftTrim"));
    QApplication::setWindowIcon(QIcon::fromTheme(QStringLiteral("audio-volume-high")));
    QApplication::setQuitOnLastWindowClosed(false);

    if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE")) {
        QQuickStyle::setStyle(QStringLiteral("org.kde.desktop"));
    }

    QQmlApplicationEngine engine;
    engine.load(QUrl(QStringLiteral("qrc:/qt/qml/org/drifttrim/app/qml/Main.qml")));
    if (engine.rootObjects().isEmpty()) {
        return 1;
    }
    return QApplication::exec();
}
