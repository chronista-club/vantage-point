import AppKit
import Foundation

// MARK: - Prompt API Types

/// Pending prompt from Process (matches PendingPromptRequest in Rust)
struct PendingPrompt: Identifiable, Codable {
    let requestId: String
    let promptType: String
    let title: String
    let description: String?
    let options: [PromptOption]?
    let defaultValue: String?
    let timeoutSeconds: UInt32
    let createdAt: UInt64

    /// Process port this prompt came from (set after decoding)
    var port: UInt16 = 33000

    var id: String {
        requestId
    }

    private enum CodingKeys: String, CodingKey {
        case title, description, options
        case requestId = "request_id"
        case promptType = "prompt_type"
        case defaultValue = "default_value"
        case timeoutSeconds = "timeout_seconds"
        case createdAt = "created_at"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        requestId = try container.decode(String.self, forKey: .requestId)
        promptType = try container.decode(String.self, forKey: .promptType)
        title = try container.decode(String.self, forKey: .title)
        description = try container.decodeIfPresent(String.self, forKey: .description)
        options = try container.decodeIfPresent([PromptOption].self, forKey: .options)
        defaultValue = try container.decodeIfPresent(String.self, forKey: .defaultValue)
        timeoutSeconds = try container.decode(UInt32.self, forKey: .timeoutSeconds)
        createdAt = try container.decode(UInt64.self, forKey: .createdAt)
        port = 33000 // Default, will be overridden
    }

    init(
        requestId: String,
        promptType: String,
        title: String,
        description: String?,
        options: [PromptOption]?,
        defaultValue: String?,
        timeoutSeconds: UInt32,
        createdAt: UInt64,
        port: UInt16
    ) {
        self.requestId = requestId
        self.promptType = promptType
        self.title = title
        self.description = description
        self.options = options
        self.defaultValue = defaultValue
        self.timeoutSeconds = timeoutSeconds
        self.createdAt = createdAt
        self.port = port
    }

    func withPort(_ port: UInt16) -> PendingPrompt {
        PendingPrompt(
            requestId: requestId,
            promptType: promptType,
            title: title,
            description: description,
            options: options,
            defaultValue: defaultValue,
            timeoutSeconds: timeoutSeconds,
            createdAt: createdAt,
            port: port
        )
    }
}

/// Prompt option for select/multi_select
struct PromptOption: Codable, Identifiable {
    let id: String
    let label: String
    let description: String?
}

/// Prompt response to send back
struct PromptResponse: Codable {
    let outcome: String
    let message: String?
    let selectedOptions: [String]?

    enum CodingKeys: String, CodingKey {
        case outcome, message
        case selectedOptions = "selected_options"
    }
}

/// Pending prompts list response
struct PendingPromptsResponse: Codable {
    let prompts: [PendingPrompt]
}

// MARK: - User Prompt Service

/// User Prompt Service
/// Polls running Processes for pending prompts and displays alerts
@MainActor
class UserPromptService: ObservableObject {
    /// Current pending prompts
    @Published var pendingPrompts: [PendingPrompt] = []

    /// Active prompt being shown
    @Published var activePrompt: PendingPrompt?

    /// URLSession for API calls
    private let session: URLSession

    /// Polling interval (seconds)
    private let pollInterval: TimeInterval = 2.0

    /// Polling timer
    private var pollTimer: Timer?

    /// Active Processes to poll (port -> last poll time)
    private var activePorts: [UInt16: Date] = [:]

    init() {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 5
        session = URLSession(configuration: config)
    }

    // MARK: - Lifecycle

    /// Start polling for prompts
    func startPolling() {
        stopPolling()
        pollTimer = Timer.scheduledTimer(
            withTimeInterval: pollInterval, repeats: true
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                await self?.pollAllProcesses()
            }
        }
    }

    /// Stop polling
    func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    /// Register a Process to poll
    func registerProcess(port: UInt16) {
        activePorts[port] = Date()
    }

    /// Unregister a Process
    func unregisterProcess(port: UInt16) {
        activePorts.removeValue(forKey: port)
    }

    /// Update active Processes from discovered list
    func updateActivePorts(ports: [UInt16]) {
        // Remove Processes that are no longer active
        for port in activePorts.keys where !ports.contains(port) {
            activePorts.removeValue(forKey: port)
        }
        // Add new Processes
        for port in ports where activePorts[port] == nil {
            activePorts[port] = Date()
        }
    }

    // MARK: - Polling

    /// Poll all active Processes for pending prompts
    private func pollAllProcesses() async {
        var allPrompts: [PendingPrompt] = []

        for port in activePorts.keys {
            if let prompts = await fetchPendingPrompts(port: port) {
                for prompt in prompts {
                    allPrompts.append(prompt.withPort(port))
                }
            }
        }

        pendingPrompts = allPrompts

        // Show alert for first pending prompt if not already showing one
        if activePrompt == nil, let first = allPrompts.first {
            await showPromptAlert(prompt: first)
        }
    }

    /// Fetch pending prompts from a Process
    private func fetchPendingPrompts(port: UInt16) async -> [PendingPrompt]? {
        let url = URL(string: "http://[::1]:\(port)/api/prompts/pending")!

        do {
            let (data, response) = try await session.data(from: url)

            guard let httpResponse = response as? HTTPURLResponse,
                  httpResponse.statusCode == 200
            else {
                return nil
            }

            let result = try JSONDecoder().decode(
                PendingPromptsResponse.self, from: data
            )
            return result.prompts
        } catch {
            // Process might not support prompts or is unavailable
            return nil
        }
    }

    // MARK: - Alert Display

    /// Show a prompt as macOS alert
    private func showPromptAlert(prompt: PendingPrompt) async {
        activePrompt = prompt

        let alert = NSAlert()
        alert.messageText = prompt.title
        alert.informativeText = prompt.description ?? ""
        alert.alertStyle = .informational

        switch prompt.promptType {
        case "confirm":
            handleConfirmPrompt(alert: alert, prompt: prompt)

        case "input":
            await handleInputPrompt(alert: alert, prompt: prompt)

        case "select":
            await handleSelectPrompt(alert: alert, prompt: prompt)

        case "multi_select":
            await handleMultiSelectPrompt(alert: alert, prompt: prompt)

        default:
            // Unknown prompt type, just show message
            alert.addButton(withTitle: "OK")
            alert.runModal()
        }

        activePrompt = nil

        // Check for more prompts after handling current one
        await pollAllProcesses()
    }

    private func handleConfirmPrompt(alert: NSAlert, prompt: PendingPrompt) {
        alert.addButton(withTitle: "Yes")
        alert.addButton(withTitle: "No")

        let response = alert.runModal()
        let outcome = response == .alertFirstButtonReturn ? "approved" : "rejected"
        Task {
            await sendResponse(
                prompt: prompt, outcome: outcome,
                message: nil, selectedOptions: nil
            )
        }
    }

    private func handleInputPrompt(alert: NSAlert, prompt: PendingPrompt) async {
        let textField = NSTextField(frame: NSRect(x: 0, y: 0, width: 300, height: 24))
        textField.stringValue = prompt.defaultValue ?? ""
        textField.placeholderString = "Enter your response..."
        alert.accessoryView = textField
        alert.addButton(withTitle: "Submit")
        alert.addButton(withTitle: "Cancel")

        let response = alert.runModal()
        if response == .alertFirstButtonReturn {
            await sendResponse(
                prompt: prompt, outcome: "approved",
                message: textField.stringValue, selectedOptions: nil
            )
        } else {
            await sendResponse(
                prompt: prompt, outcome: "cancelled",
                message: nil, selectedOptions: nil
            )
        }
    }

    private func handleSelectPrompt(alert: NSAlert, prompt: PendingPrompt) async {
        guard let options = prompt.options else { return }

        let popup = NSPopUpButton(
            frame: NSRect(x: 0, y: 0, width: 300, height: 28),
            pullsDown: false
        )
        for option in options {
            popup.addItem(withTitle: option.label)
            popup.lastItem?.representedObject = option.id
        }
        alert.accessoryView = popup
        alert.addButton(withTitle: "Select")
        alert.addButton(withTitle: "Cancel")

        let response = alert.runModal()
        if response == .alertFirstButtonReturn {
            if let selectedId = popup.selectedItem?.representedObject as? String {
                await sendResponse(
                    prompt: prompt, outcome: "approved",
                    message: nil, selectedOptions: [selectedId]
                )
            }
        } else {
            await sendResponse(
                prompt: prompt, outcome: "cancelled",
                message: nil, selectedOptions: nil
            )
        }
    }

    private func handleMultiSelectPrompt(alert: NSAlert, prompt: PendingPrompt) async {
        guard let options = prompt.options else { return }

        let height = CGFloat(options.count * 24)
        let stackView = NSStackView(
            frame: NSRect(x: 0, y: 0, width: 300, height: height)
        )
        stackView.orientation = .vertical
        stackView.alignment = .leading
        stackView.spacing = 4

        var checkboxes: [(NSButton, String)] = []
        for option in options {
            let checkbox = NSButton(
                checkboxWithTitle: option.label,
                target: nil, action: nil
            )
            checkbox.state = .off
            stackView.addArrangedSubview(checkbox)
            checkboxes.append((checkbox, option.id))
        }

        alert.accessoryView = stackView
        alert.addButton(withTitle: "Submit")
        alert.addButton(withTitle: "Cancel")

        let response = alert.runModal()
        if response == .alertFirstButtonReturn {
            let selectedIds = checkboxes
                .filter { $0.0.state == .on }
                .map(\.1)
            await sendResponse(
                prompt: prompt, outcome: "approved",
                message: nil, selectedOptions: selectedIds
            )
        } else {
            await sendResponse(
                prompt: prompt, outcome: "cancelled",
                message: nil, selectedOptions: nil
            )
        }
    }

    // MARK: - Response

    /// Send response back to Process
    private func sendResponse(
        prompt: PendingPrompt, outcome: String,
        message: String?, selectedOptions: [String]?
    ) async {
        let url = URL(string: "http://[::1]:\(prompt.port)/api/prompt/\(prompt.requestId)")!
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")

        let response = PromptResponse(
            outcome: outcome,
            message: message,
            selectedOptions: selectedOptions
        )

        do {
            request.httpBody = try JSONEncoder().encode(response)
            let (_, httpResponse) = try await session.data(for: request)

            if let httpResp = httpResponse as? HTTPURLResponse, httpResp.statusCode != 200 {
                print("UserPromptService: Failed to send response, status: \(httpResp.statusCode)")
            }
        } catch {
            print("UserPromptService: Failed to send response: \(error)")
        }
    }
}
