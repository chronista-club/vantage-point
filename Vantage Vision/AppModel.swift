//
//  AppModel.swift
//  Vantage
//
//  Created by Makoto Itoh on 2025/07/03.
//

import SwiftUI

/// Maintains app-wide state
@MainActor
@Observable
class AppModel {
    let immersiveSpaceID = "ImmersiveSpace"
    enum ImmersiveSpaceState {
        case closed
        case inTransition
        case open
    }
    var immersiveSpaceState = ImmersiveSpaceState.closed
    
    /// AIアシスタントモデル
    let aiAssistant = AIAssistantModel()
    
    /// AIアシスタントウィンドウの表示位置
    var aiAssistantPosition = SIMD3<Float>(x: 0.5, y: 0, z: -1.5)
    
    /// ウィンドウを開くための環境アクション
    var openWindow: OpenWindowAction?
    
    /// ウィンドウを閉じるための環境アクション  
    var dismissWindow: DismissWindowAction?
    
    /// AIアシスタントの表示を切り替え
    func toggleAIAssistant() {
        withAnimation(.spring(response: 0.3, dampingFraction: 0.8)) {
            if aiAssistant.isShowing {
                dismissWindow?(id: "AIAssistant")
                aiAssistant.isShowing = false
            } else {
                openWindow?(id: "AIAssistant")
                aiAssistant.isShowing = true
            }
        }
    }
}
