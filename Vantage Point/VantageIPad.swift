//
//  VantageIPad.swift
//  Vantage Point for iPad
//
//  Created by Claude Code on 2025/08/01.
//

import SwiftUI

@main
struct VantageIPadApp: App {
    @StateObject private var chatViewModel = ChatViewModel()
    
    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(chatViewModel)
        }
    }
}