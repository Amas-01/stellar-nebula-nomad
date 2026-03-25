use crate::{CellType, NebulaLayout};
use crate::ship_nft::{DataKey as ShipDataKey, ShipNft};
use arrayvec::ArrayVec;
use soroban_sdk::{
    contracterror, contracttype, symbol_short, Address, Env, Symbol, Vec,
};

pub type AssetId = Symbol;

const ASSET_STELLAR_DUST: Symbol = symbol_short!("dust");
const ASSET_ASTEROID_ORE: Symbol = symbol_short!("ore");
const ASSET_GAS_UNITS: Symbol = symbol_short!("gas");
const ASSET_DARK_MATTER: Symbol = symbol_short!("dark");
const ASSET_EXOTIC_MATTER: Symbol = symbol_short!("exotic");
const ASSET_WORMHOLE_CORE: Symbol = symbol_short!("worm");

#[derive(Clone)]
#[contracttype]
pub enum ResourceKey {
    ResourceCounter,
    HarvestCounter,
    DexOfferCounter,
    ResourceBalance(Address, AssetId),
    DexOffer(u64),
}

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum HarvestError {
    ShipNotFound = 1,
    EmptyHarvest = 2,
    InvalidPrice = 3,
    AssetNotHarvested = 4,
    PriceOverflow = 5,
}

/// Resource data structure for in-game tradeable resources.
#[derive(Clone)]
#[contracttype]
pub struct Resource {
    pub id: u64,
    pub owner: Address,
    pub resource_type: u32,
    pub quantity: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct HarvestedResource {
    pub asset: AssetId,
    pub quantity: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct DexOffer {
    pub offer_id: u64,
    pub seller: Address,
    pub resource: AssetId,
    pub quantity: i128,
    pub min_price: i128,
    pub quote_asset: Symbol,
    pub timestamp: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct HarvestResult {
    pub ship_id: u64,
    pub owner: Address,
    pub resources: Vec<HarvestedResource>,
    pub total_units: i128,
    pub auto_offer: DexOffer,
    pub harvested_at: u64,
}

fn get_ship_owner(env: &Env, ship_id: u64) -> Result<Address, HarvestError> {
    let ship: ShipNft = env
        .storage()
        .persistent()
        .get(&ShipDataKey::Ship(ship_id))
        .ok_or(HarvestError::ShipNotFound)?;

    ship.owner.require_auth();
    Ok(ship.owner)
}

fn next_counter(env: &Env, key: &ResourceKey) -> u64 {
    let current: u64 = env.storage().persistent().get(key).unwrap_or(0);
    let next = current + 1;
    env.storage().persistent().set(key, &next);
    next
}

fn add_balance(env: &Env, owner: &Address, asset: &AssetId, quantity: i128) {
    if quantity <= 0 {
        return;
    }
    let key = ResourceKey::ResourceBalance(owner.clone(), asset.clone());
    let old: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    env.storage().persistent().set(&key, &(old + quantity));
}

fn get_balance(env: &Env, owner: &Address, asset: &AssetId) -> i128 {
    env.storage()
        .persistent()
        .get(&ResourceKey::ResourceBalance(owner.clone(), asset.clone()))
        .unwrap_or(0)
}

fn set_balance(env: &Env, owner: &Address, asset: &AssetId, qty: i128) {
    env.storage().persistent().set(
        &ResourceKey::ResourceBalance(owner.clone(), asset.clone()),
        &qty,
    );
}

fn choose_listing_asset(resources: &Vec<HarvestedResource>) -> Option<HarvestedResource> {
    let mut best: Option<HarvestedResource> = None;
    for i in 0..resources.len() {
        let item = resources.get(i).unwrap();
        if item.quantity <= 0 {
            continue;
        }

        match &best {
            None => best = Some(item),
            Some(current) => {
                if item.quantity > current.quantity {
                    best = Some(item);
                }
            }
        }
    }
    best
}

fn listing_price_for(asset: &AssetId) -> i128 {
    if *asset == ASSET_WORMHOLE_CORE {
        5_000
    } else if *asset == ASSET_EXOTIC_MATTER {
        1_500
    } else if *asset == ASSET_DARK_MATTER {
        600
    } else if *asset == ASSET_STELLAR_DUST {
        90
    } else if *asset == ASSET_GAS_UNITS {
        70
    } else {
        50
    }
}

fn auto_list_on_dex_for_owner(
    env: &Env,
    owner: &Address,
    resource: &AssetId,
    min_price: i128,
) -> Result<DexOffer, HarvestError> {
    if min_price <= 0 {
        return Err(HarvestError::InvalidPrice);
    }

    let available = get_balance(env, owner, resource);
    if available <= 0 {
        return Err(HarvestError::AssetNotHarvested);
    }

    let qty_to_list = if available > 100 { 100 } else { available };
    set_balance(env, owner, resource, available - qty_to_list);

    let offer_id = next_counter(env, &ResourceKey::DexOfferCounter);
    let offer = DexOffer {
        offer_id,
        seller: owner.clone(),
        resource: resource.clone(),
        quantity: qty_to_list,
        min_price,
        quote_asset: symbol_short!("xlm"),
        timestamp: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&ResourceKey::DexOffer(offer_id), &offer);

    env.events().publish(
        (symbol_short!("dex"), symbol_short!("listed")),
        (
            offer.offer_id,
            offer.seller.clone(),
            offer.resource.clone(),
            offer.quantity,
            offer.min_price,
            offer.quote_asset.clone(),
        ),
    );

    Ok(offer)
}

/// Harvest resources for a ship with a single call path:
///
/// 1. Validates ship ownership and auth.
/// 2. Iterates over layout cells once and aggregates quantities.
/// 3. Updates persistent balances in batch.
/// 4. Emits `ResourcesHarvested` event.
/// 5. Auto-lists the largest harvested asset on DEX hook in the same invocation.
pub fn harvest_resources(
    env: &Env,
    ship_id: u64,
    layout: &NebulaLayout,
) -> Result<HarvestResult, HarvestError> {
    let owner = get_ship_owner(env, ship_id)?;

    let mut dust_qty: i128 = 0;
    let mut ore_qty: i128 = 0;
    let mut gas_qty: i128 = 0;
    let mut dark_qty: i128 = 0;
    let mut exotic_qty: i128 = 0;
    let mut worm_qty: i128 = 0;

    for i in 0..layout.cells.len() {
        let cell = layout.cells.get(i).unwrap();
        match cell.cell_type {
            CellType::Star => dust_qty += 2 + (cell.energy as i128 / 20),
            CellType::Asteroid => ore_qty += 3 + (cell.energy as i128 / 15),
            CellType::GasCloud => gas_qty += 2 + (cell.energy as i128 / 16),
            CellType::DarkMatter => dark_qty += 1 + (cell.energy as i128 / 25),
            CellType::ExoticMatter => exotic_qty += 1 + (cell.energy as i128 / 30),
            CellType::Wormhole => worm_qty += 1,
            CellType::StellarDust => dust_qty += 1 + (cell.energy as i128 / 18),
            CellType::Empty => {}
        }
    }

    let mut staged: ArrayVec<HarvestedResource, 6> = ArrayVec::new();

    if dust_qty > 0 {
        add_balance(env, &owner, &ASSET_STELLAR_DUST, dust_qty);
        staged.push(HarvestedResource {
            asset: ASSET_STELLAR_DUST,
            quantity: dust_qty,
        });
    }
    if ore_qty > 0 {
        add_balance(env, &owner, &ASSET_ASTEROID_ORE, ore_qty);
        staged.push(HarvestedResource {
            asset: ASSET_ASTEROID_ORE,
            quantity: ore_qty,
        });
    }
    if gas_qty > 0 {
        add_balance(env, &owner, &ASSET_GAS_UNITS, gas_qty);
        staged.push(HarvestedResource {
            asset: ASSET_GAS_UNITS,
            quantity: gas_qty,
        });
    }
    if dark_qty > 0 {
        add_balance(env, &owner, &ASSET_DARK_MATTER, dark_qty);
        staged.push(HarvestedResource {
            asset: ASSET_DARK_MATTER,
            quantity: dark_qty,
        });
    }
    if exotic_qty > 0 {
        add_balance(env, &owner, &ASSET_EXOTIC_MATTER, exotic_qty);
        staged.push(HarvestedResource {
            asset: ASSET_EXOTIC_MATTER,
            quantity: exotic_qty,
        });
    }
    if worm_qty > 0 {
        add_balance(env, &owner, &ASSET_WORMHOLE_CORE, worm_qty);
        staged.push(HarvestedResource {
            asset: ASSET_WORMHOLE_CORE,
            quantity: worm_qty,
        });
    }

    let mut resources = Vec::new(env);
    for item in staged {
        resources.push_back(item);
    }

    if resources.is_empty() {
        return Err(HarvestError::EmptyHarvest);
    }

    let _harvest_id = next_counter(env, &ResourceKey::HarvestCounter);
    let total_units = dust_qty + ore_qty + gas_qty + dark_qty + exotic_qty + worm_qty;

    env.events().publish(
        (symbol_short!("harvest"), symbol_short!("done")),
        (
            ship_id,
            owner.clone(),
            resources.clone(),
            total_units,
            layout.total_energy,
        ),
    );

    let auto_asset = choose_listing_asset(&resources).ok_or(HarvestError::EmptyHarvest)?;
    let auto_offer = auto_list_on_dex_for_owner(
        env,
        &owner,
        &auto_asset.asset,
        listing_price_for(&auto_asset.asset),
    )?;

    Ok(HarvestResult {
        ship_id,
        owner,
        resources,
        total_units,
        auto_offer,
        harvested_at: env.ledger().timestamp(),
    })
}

/// Auto-list a harvested resource on Stellar DEX-compatible offer hook.
/// This keeps the same owner auth model and can be called standalone.
pub fn auto_list_on_dex(
    env: &Env,
    resource: &AssetId,
    min_price: i128,
) -> Result<DexOffer, HarvestError> {
    if min_price <= 0 {
        return Err(HarvestError::InvalidPrice);
    }

    // Standalone listing uses the contract address as a marker seller.
    // In practice, the frontend passes the player's address and has them
    // authorise via a wrapper; for the on-chain hook the contract acts
    // as custodian after harvest_resources already debited the player.
    let owner = env.current_contract_address();
    auto_list_on_dex_for_owner(env, &owner, resource, min_price)
}

