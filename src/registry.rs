//! Adapter registry — one place that maps a channel onto its adapter.

use std::collections::HashMap;
use std::fmt;

use crate::error::{Error, Result};
use crate::model::{Channel, NormalizedMessage, ProviderPayload};
use crate::{Adapter, Headers};

/// A set of adapters, one per channel.
///
/// Registration is by [`Adapter::channel`]; looking up a channel nobody
/// registered is a typed error, never a panic. The registry is immutable after
/// setup, so it can be shared across tasks.
///
/// ```
/// # #[cfg(feature = "whatsapp")]
/// # fn demo() -> Result<(), messenger_webhook::Error> {
/// use messenger_webhook::{Adapter, Channel, Registry, WhatsAppAdapter};
///
/// let mut registry = Registry::new();
/// registry.register(WhatsAppAdapter::new("app-secret"));
///
/// assert_eq!(registry.get(&Channel::WhatsApp)?.channel(), Channel::WhatsApp);
/// assert!(registry.get(&Channel::Matrix).is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct Registry {
    adapters: HashMap<Channel, Box<dyn Adapter>>,
}

impl Registry {
    /// Creates an empty registry.
    ///
    /// ```
    /// use messenger_webhook::Registry;
    ///
    /// assert!(Registry::new().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `adapter` under its channel and returns `self`, so calls chain.
    ///
    /// Registering a second adapter for the same channel replaces the first;
    /// that keeps configuration code (defaults, then overrides) simple.
    ///
    /// ```
    /// # #[cfg(all(feature = "whatsapp", feature = "telegram"))]
    /// # fn demo() {
    /// use messenger_webhook::{Adapter, Channel, Registry, TelegramAdapter, WhatsAppAdapter};
    ///
    /// let mut registry = Registry::new();
    /// registry
    ///     .register(WhatsAppAdapter::new("app-secret"))
    ///     .register(TelegramAdapter::new("bot-token"));
    ///
    /// assert_eq!(registry.len(), 2);
    /// # }
    /// ```
    pub fn register<A: Adapter + 'static>(&mut self, adapter: A) -> &mut Self {
        self.adapters.insert(adapter.channel(), Box::new(adapter));
        self
    }

    /// Looks the adapter for `channel` up.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownChannel`] when nothing is registered for
    /// `channel`.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Registry};
    ///
    /// let registry = Registry::new();
    /// assert!(registry.get(&Channel::Other("signal".into())).is_err());
    /// ```
    pub fn get(&self, channel: &Channel) -> Result<&dyn Adapter> {
        self.adapters
            .get(channel)
            .map(AsRef::as_ref)
            .ok_or_else(|| Error::UnknownChannel(channel.clone()))
    }

    /// Whether an adapter is registered for `channel`.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Registry};
    ///
    /// assert!(!Registry::new().contains(&Channel::WhatsApp));
    /// ```
    #[must_use]
    pub fn contains(&self, channel: &Channel) -> bool {
        self.adapters.contains_key(channel)
    }

    /// Every registered channel, sorted.
    ///
    /// ```
    /// # #[cfg(feature = "whatsapp")]
    /// # fn demo() {
    /// use messenger_webhook::{Channel, Registry, WhatsAppAdapter};
    ///
    /// let mut registry = Registry::new();
    /// registry.register(WhatsAppAdapter::new("s"));
    /// assert_eq!(registry.channels(), vec![Channel::WhatsApp]);
    /// # }
    /// ```
    #[must_use]
    pub fn channels(&self) -> Vec<Channel> {
        let mut channels: Vec<Channel> = self.adapters.keys().cloned().collect();
        channels.sort();
        channels
    }

    /// Number of registered adapters.
    ///
    /// ```
    /// use messenger_webhook::Registry;
    ///
    /// assert_eq!(Registry::new().len(), 0);
    /// ```
    #[must_use]
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Whether no adapter is registered.
    ///
    /// ```
    /// use messenger_webhook::Registry;
    ///
    /// assert!(Registry::new().is_empty());
    /// ```
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Verifies a request against the adapter for `channel`.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChannel`] if the channel has no adapter, plus every
    /// error [`Adapter::verify`] reports.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Headers, Registry};
    ///
    /// assert!(Registry::new().verify(&Channel::Telegram, &Headers::new(), b"{}").is_err());
    /// ```
    pub fn verify(&self, channel: &Channel, headers: &Headers, body: &[u8]) -> Result<()> {
        self.get(channel)?.verify(headers, body)
    }

    /// Parses a payload with the adapter for `channel`.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChannel`] if the channel has no adapter, plus every
    /// error [`Adapter::adapt`] reports.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Registry};
    ///
    /// assert!(Registry::new().adapt(&Channel::Matrix, b"{}").is_err());
    /// ```
    pub fn adapt(&self, channel: &Channel, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        self.get(channel)?.adapt(payload)
    }

    /// Verifies, then parses — the safe default path for an inbound webhook.
    ///
    /// The body is never parsed before it is proven authentic.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChannel`] if the channel has no adapter, plus every
    /// error [`Adapter::verify`] or [`Adapter::adapt`] reports.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Headers, Registry};
    ///
    /// // No adapter registered: typed error, no panic.
    /// let error = Registry::new().handle(&Channel::Matrix, &Headers::new(), b"{}").unwrap_err();
    /// assert!(matches!(error, messenger_webhook::Error::UnknownChannel(_)));
    /// ```
    pub fn handle(
        &self,
        channel: &Channel,
        headers: &Headers,
        body: &[u8],
    ) -> Result<Vec<NormalizedMessage>> {
        let adapter = self.get(channel)?;
        adapter.verify(headers, body)?;
        adapter.adapt(body)
    }

    /// Encodes `message` for `channel`.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChannel`] if the channel has no adapter, plus every
    /// error [`Adapter::encode`] reports.
    ///
    /// ```
    /// use messenger_webhook::{Channel, NormalizedMessage, Registry};
    ///
    /// let registry = Registry::new();
    /// let message = NormalizedMessage::text(Channel::Matrix, "!r:s", "hi");
    /// assert!(registry.encode(&Channel::Matrix, &message).is_err());
    /// ```
    pub fn encode(
        &self,
        channel: &Channel,
        message: &NormalizedMessage,
    ) -> Result<ProviderPayload> {
        self.get(channel)?.encode(message)
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("channels", &self.channels())
            .finish()
    }
}
