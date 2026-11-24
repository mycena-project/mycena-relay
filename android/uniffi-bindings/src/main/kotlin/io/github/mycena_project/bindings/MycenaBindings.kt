// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
package io.github.mycena_project.bindings

typealias NodeConfig = uniffi.mycena.NodeConfig
typealias ChatMessage = uniffi.mycena.ChatMessage
typealias BulletinPost = uniffi.mycena.BulletinPost
typealias MailMessage = uniffi.mycena.MailMessage
typealias NodeMetrics = uniffi.mycena.NodeMetrics
typealias WalletBalance = uniffi.mycena.WalletBalance
typealias TxInfo = uniffi.mycena.TxInfo
typealias EnergyState = uniffi.mycena.EnergyState
typealias NodeEventCallback = uniffi.mycena.NodeEventCallback
typealias ConnectivityMode = uniffi.mycena.ConnectivityMode
typealias HotWalletConfig = uniffi.mycena.HotWalletConfig
typealias HotWalletStatus = uniffi.mycena.HotWalletStatus
typealias PaymentRequestData = uniffi.mycena.PaymentRequestData

object mycena {
    fun initNode(config: NodeConfig) = uniffi.mycena.initNode(config)
    fun stopNode() = uniffi.mycena.stopNode()
    fun localPeerId(): String = uniffi.mycena.localPeerId()
    fun knownPeers(): List<String> = uniffi.mycena.knownPeers()
    fun metrics(): NodeMetrics = uniffi.mycena.metrics()
    fun sendChatDirect(toPeer: String, body: String) = uniffi.mycena.sendChatDirect(toPeer, body)
    fun sendChatBroadcast(body: String) = uniffi.mycena.sendChatBroadcast(body)
    fun chatHistory(peerId: String, limit: UInt): List<ChatMessage> = uniffi.mycena.chatHistory(peerId, limit)
    fun postBulletin(scope: String, title: String, body: String, ttlSecs: ULong) =
        uniffi.mycena.postBulletin(scope, title, body, ttlSecs)
    fun bulletinsForScope(scope: String): List<BulletinPost> = uniffi.mycena.bulletinsForScope(scope)
    fun sendMail(toPeer: String, subject: String, body: String) = uniffi.mycena.sendMail(toPeer, subject, body)
    fun mailInbox(limit: UInt): List<MailMessage> = uniffi.mycena.mailInbox(limit)
    fun mailSent(limit: UInt): List<MailMessage> = uniffi.mycena.mailSent(limit)
    fun markMailRead(mailId: String) = uniffi.mycena.markMailRead(mailId)
    fun setDisplayName(name: String) = uniffi.mycena.setDisplayName(name)
    fun displayName(): String = uniffi.mycena.displayName()
    fun setEnergyState(state: EnergyState) = uniffi.mycena.setEnergyState(state)
    fun addBootstrapPeer(multiaddr: String) = uniffi.mycena.addBootstrapPeer(multiaddr)
    fun setEventCallback(callback: NodeEventCallback) = uniffi.mycena.setEventCallback(callback)
    fun walletAddress(): String = uniffi.mycena.walletAddress()
    fun walletBalance(): WalletBalance = uniffi.mycena.walletBalance()
    fun sendTransaction(toAddress: String, amountMuon: ULong, memo: String?) =
        uniffi.mycena.sendTransaction(toAddress, amountMuon, memo)
    fun walletRecentTransactions(limit: UInt): List<TxInfo> =
        uniffi.mycena.walletRecentTransactions(limit)
    fun requestFaucet() = uniffi.mycena.requestFaucet()
    fun currentConnectivityMode(): ConnectivityMode = uniffi.mycena.currentConnectivityMode()
    fun getHotWalletConfig(): HotWalletConfig = uniffi.mycena.getHotWalletConfig()
    fun setHotWalletConfig(config: HotWalletConfig) = uniffi.mycena.setHotWalletConfig(config)
    fun setColdWalletAddress(address: String?) = uniffi.mycena.setColdWalletAddress(address)
    fun hotWalletStatus(): HotWalletStatus = uniffi.mycena.hotWalletStatus()
    fun buildPaymentRequestUri(toAddress: String, amountMuon: ULong, memo: String?): String =
        uniffi.mycena.buildPaymentRequestUri(toAddress, amountMuon, memo)
    fun parsePaymentRequestUri(uri: String): PaymentRequestData? =
        uniffi.mycena.parsePaymentRequestUri(uri)
}
