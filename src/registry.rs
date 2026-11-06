//! Adapter registry — one place that maps a channel onto its adapter.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::model::{Channel, NormalizedMessage, OutboundMessage, ProviderPayload};
use crate::{check_body, Adapter, Headers};

/// A set of adapters, one per channel.
///
/// Registration is by [`Adapter::channel`]; looking up a channel nobody
/// registered is a typed error, never a panic. The registry is immutable after
/// setup, so it can be shared across tasks. Lookups return an owned
/// [`Arc<dyn Adapter>`], so callers can drop any lock around a registry before
/// dispatching to the adapter.
///
/// ```
/// # #[cfg(feature = "whatsapp")]
/// # fn demo() -> Result<(), messenger_webhook::Error> {
/// use messenger_webhook::{Adapter, Channel, Registry, WhatsAppAdapter};
///
/// let mut registry = Registry::new();
/// registry.register(WhatsAppAdapter::new("app-secret-app-secret")?);
///
/// assert_eq!(registry.get(&Channel::WhatsApp)?.channel(), Channel::WhatsApp);
/// assert!(registry.get(&Channel::Matrix).is_err());
/// # Ok(())
/// # }
/// ```
#[derive(Default)]
pub struct Registry {
    adapters: BTreeMap<Channel, Arc<dyn Adapter>>,
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
    /// that keeps configuration code (defaults, then overrides) simple. Use
    /// [`Registry::try_register`] when a duplicate registration should be an
    /// error instead.
    ///
    /// ```
    /// # #[cfg(all(feature = "whatsapp", feature = "telegram"))]
    /// # fn demo() -> Result<(), messenger_webhook::Error> {
    /// use messenger_webhook::{Adapter, Channel, Registry, TelegramAdapter, WhatsAppAdapter};
    ///
    /// let mut registry = Registry::new();
    /// registry
    ///     .register(WhatsAppAdapter::new("app-secret-app-secret")?)
    ///     .register(TelegramAdapter::new("bot-token-bot-token")?);
    ///
    /// assert_eq!(registry.len(), 2);
    /// # Ok(())
    /// # }
    /// ```
    pub fn register<A: Adapter + 'static>(&mut self, adapter: A) -> &mut Self {
        self.adapters.insert(adapter.channel(), Arc::new(adapter));
        self
    }

    /// Registers an already shared `adapter` under its channel.
    ///
    /// Registering a second adapter for the same channel replaces the first;
    /// that keeps runtime configuration code simple. Use [`Registry::contains`]
    /// before calling this method when a duplicate shared registration should
    /// not replace the existing adapter.
    ///
    /// ```
    /// # #[cfg(feature = "matrix")]
    /// # fn demo() -> Result<(), messenger_webhook::Error> {
    /// use std::sync::Arc;
    /// use messenger_webhook::{Adapter, Channel, MatrixAdapter, Registry};
    ///
    /// let adapter: Arc<dyn Adapter> = Arc::new(MatrixAdapter::new("token-123456789")?);
    /// let mut registry = Registry::new();
    /// registry.register_shared(adapter);
    ///
    /// assert_eq!(
    ///     registry.get(&Channel::Matrix)?.channel(),
    ///     Channel::Matrix
    /// );
    /// # Ok(())
    /// # }
    /// ```
    pub fn register_shared(&mut self, adapter: Arc<dyn Adapter>) -> &mut Self {
        self.adapters.insert(adapter.channel(), adapter);
        self
    }

    /// Registers `adapter` unless its channel already has an adapter.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ChannelAlreadyRegistered`] when `adapter`'s channel is
    /// already present. The existing adapter is kept unchanged.
    ///
    /// ```
    /// # #[cfg(feature = "matrix")]
    /// # fn demo() -> Result<(), messenger_webhook::Error> {
    /// use messenger_webhook::{Channel, Error, MatrixAdapter, Registry};
    ///
    /// let mut registry = Registry::new();
    /// registry.try_register(MatrixAdapter::new("secret-secret-secret")?)?;
    ///
    /// let error = registry
    ///     .try_register(MatrixAdapter::new("other-token-other-token")?)
    ///     .unwrap_err();
    /// assert!(matches!(error, Error::ChannelAlreadyRegistered(Channel::Matrix)));
    /// # Ok(())
    /// # }
    /// ```
    pub fn try_register<A: Adapter + 'static>(&mut self, adapter: A) -> Result<()> {
        let channel = adapter.channel();
        if self.adapters.contains_key(&channel) {
            return Err(Error::ChannelAlreadyRegistered(channel));
        }
        self.adapters.insert(channel, Arc::new(adapter));
        Ok(())
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
    /// assert!(registry.get(&Channel::other("signal")?).is_err());
    /// # Ok::<(), messenger_webhook::Error>(())
    /// ```
    pub fn get(&self, channel: &Channel) -> Result<Arc<dyn Adapter>> {
        self.adapters
            .get(channel)
            .cloned()
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
    /// # fn demo() -> Result<(), messenger_webhook::Error> {
    /// use messenger_webhook::{Channel, Registry, WhatsAppAdapter};
    ///
    /// let mut registry = Registry::new();
    /// registry.register(WhatsAppAdapter::new("secret-secret-secret")?);
    /// assert_eq!(registry.channels(), vec![Channel::WhatsApp]);
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn channels(&self) -> Vec<Channel> {
        self.adapters.keys().cloned().collect()
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
    /// error [`Adapter::verify`] reports. A body above the registered adapter's
    /// [`Adapter::max_body_bytes`] is rejected before the adapter is called.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Headers, Registry};
    ///
    /// assert!(Registry::new().verify(&Channel::Telegram, &Headers::new(), b"{}").is_err());
    /// ```
    pub fn verify(&self, channel: &Channel, headers: &Headers, body: &[u8]) -> Result<()> {
        let adapter = self.get(channel)?;
        check_body(body.len(), adapter.max_body_bytes())?;
        adapter.verify(headers, body)
    }

    /// Parses a payload with the adapter for `channel`.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChannel`] if the channel has no adapter, plus every
    /// error [`Adapter::adapt`] reports. A payload above the registered
    /// adapter's [`Adapter::max_body_bytes`] is rejected before the adapter is
    /// called.
    ///
    /// ```
    /// use messenger_webhook::{Channel, Registry};
    ///
    /// assert!(Registry::new().adapt(&Channel::Matrix, b"{}").is_err());
    /// ```
    pub fn adapt(&self, channel: &Channel, payload: &[u8]) -> Result<Vec<NormalizedMessage>> {
        let adapter = self.get(channel)?;
        check_body(payload.len(), adapter.max_body_bytes())?;
        adapter.adapt(payload)
    }

    /// Verifies, then parses — the safe default path for an inbound webhook.
    ///
    /// The body is never parsed before it is proven authentic.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChannel`] if the channel has no adapter, plus every
    /// error [`Adapter::verify`] or [`Adapter::adapt`] reports. A body above
    /// the registered adapter's [`Adapter::max_body_bytes`] is rejected before
    /// either adapter method is called.
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
        check_body(body.len(), adapter.max_body_bytes())?;
        adapter.verify(headers, body)?;
        adapter.adapt(body)
    }

    /// Encodes `message` for `channel`.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownChannel`] if the channel has no adapter,
    /// [`Error::InvalidMessage`] if `message` was not normalised from the same
    /// channel, plus every error [`Adapter::encode`] reports.
    ///
    /// ```
    /// use messenger_webhook::{Channel, OutboundMessage, Registry};
    ///
    /// let registry = Registry::new();
    /// let message = OutboundMessage::text(Channel::Matrix, "!r:s", "hi");
    /// assert!(registry.encode(&Channel::Matrix, &message).is_err());
    /// ```
    pub fn encode(&self, channel: &Channel, message: &OutboundMessage) -> Result<ProviderPayload> {
        let adapter = self.get(channel)?;
        if message.channel != *channel {
            return Err(Error::InvalidMessage {
                channel: channel.clone(),
                reason: format!("message channel is {}, not {}", message.channel, channel),
            });
        }
        adapter.encode(message)
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry")
            .field("adapters", &DebugAdapters(self))
            .finish()
    }
}

struct DebugAdapters<'a>(&'a Registry);

impl fmt::Debug for DebugAdapters<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.adapters.iter()).finish()
    }
}
