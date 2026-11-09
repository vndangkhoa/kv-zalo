import AppKit

// `Olaz --engine-probe [login|main] [seconds]` runs the runtime harness;
// otherwise the SwiftUI app starts.
if CommandLine.arguments.contains("--engine-probe") {
    MainActor.assumeIsolated {
        EngineProbe.run(arguments: Array(CommandLine.arguments.dropFirst()))
    }
} else {
    // Before anything opens cookies, WebKit storage or preferences: they are keyed by bundle id.
    MainActor.assumeIsolated { LegacyMigration.runIfNeeded() }
    OlazApp.main()
}
