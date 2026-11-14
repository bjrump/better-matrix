//! Making and naming rooms and spaces: a new room (in a space, or on its own), a new space, and
//! a new name for either.

use matrix_sdk::ruma::api::client::room::create_room::v3::{CreationContent, Request as CreateRoom, RoomPreset};
use matrix_sdk::ruma::events::InitialStateEvent;
use matrix_sdk::ruma::events::room::encryption::RoomEncryptionEventContent;
use matrix_sdk::ruma::events::space::child::SpaceChildEventContent;
use matrix_sdk::ruma::events::space::parent::SpaceParentEventContent;
use matrix_sdk::ruma::room::RoomType;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};

use crate::Error;
use crate::account::Account;

impl Account {
    /// Creates a private, encrypted room, inside `space` when given (the room lists it as its
    /// parent and the space as its child); returns its ID.
    pub async fn create_room(&self, name: &str, topic: Option<&str>, space: Option<&str>) -> Result<String, Error> {
        let mut request = CreateRoom::new();
        request.name = Some(name.trim().to_owned());
        request.topic = topic.map(str::trim).filter(|topic| !topic.is_empty()).map(ToOwned::to_owned);
        request.preset = Some(RoomPreset::PrivateChat);
        let encryption =
            InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults());
        request.initial_state = vec![Raw::new(&encryption).map_err(|e| Error::Storage(e.into()))?.cast_unchecked()];
        let room = self.client().create_room(request).await?;
        if let Some(space) = space {
            let space_id = RoomId::parse(space)?;
            let via = vec![self.user_id()?.server_name().to_owned()];
            let space_room = self
                .client()
                .get_room(&space_id)
                .ok_or_else(|| Error::UnknownRoom { account: self.id().clone(), room_id: space.to_owned() })?;
            space_room.send_state_event_for_key(room.room_id(), SpaceChildEventContent::new(via.clone())).await?;
            let parent: OwnedRoomId = space_id;
            room.send_state_event_for_key(&parent, SpaceParentEventContent::new(via)).await?;
        }
        Ok(room.room_id().to_string())
    }

    /// Creates a space; returns its ID.
    pub async fn create_space(&self, name: &str) -> Result<String, Error> {
        let mut request = CreateRoom::new();
        request.name = Some(name.trim().to_owned());
        request.preset = Some(RoomPreset::PrivateChat);
        let mut creation = CreationContent::new();
        creation.room_type = Some(RoomType::Space);
        request.creation_content = Some(Raw::new(&creation).map_err(|e| Error::Storage(e.into()))?);
        Ok(self.client().create_room(request).await?.room_id().to_string())
    }

    /// Gives a room or space a new name.
    pub async fn rename_room(&self, room_id: &str, name: &str) -> Result<(), Error> {
        let room = self
            .client()
            .get_room(&RoomId::parse(room_id)?)
            .ok_or_else(|| Error::UnknownRoom { account: self.id().clone(), room_id: room_id.to_owned() })?;
        room.set_name(name.trim().to_owned()).await?;
        Ok(())
    }
}
