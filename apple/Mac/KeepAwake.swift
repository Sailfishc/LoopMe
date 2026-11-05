import Foundation
import IOKit.ps
import IOKit.pwr_mgt

/// Keeps the Mac from idle-sleeping while on AC power, so scheduled checks run.
@MainActor
final class KeepAwake: ObservableObject {
    @Published var enabled: Bool {
        didSet {
            UserDefaults.standard.set(enabled, forKey: "keepAwakeOnAC")
            update()
        }
    }
    @Published private(set) var active = false

    private var assertion: IOPMAssertionID = 0
    private var timer: Timer?

    init() {
        enabled = UserDefaults.standard.object(forKey: "keepAwakeOnAC") as? Bool ?? true
    }

    func start() {
        update()
        timer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.update() }
        }
    }

    private func onACPower() -> Bool {
        let info = IOPSCopyPowerSourcesInfo().takeRetainedValue()
        guard let type = IOPSGetProvidingPowerSourceType(info)?.takeUnretainedValue() else { return true }
        return (type as String) == kIOPMACPowerKey
    }

    func update() {
        let want = enabled && onACPower()
        if want && !active {
            let r = IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleSystemSleep as CFString,
                                                IOPMAssertionLevel(kIOPMAssertionLevelOn),
                                                "LoopMe 正在跟进事项" as CFString,
                                                &assertion)
            active = r == kIOReturnSuccess
        } else if !want && active {
            IOPMAssertionRelease(assertion)
            active = false
        }
    }
}
