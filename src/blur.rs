//! A Gaussian blur for Windows Composition. Composition takes its effects as
//! descriptions (`IGraphicsEffect`) that it compiles into a brush; Win2D usually
//! provides them, so here is the one we need, described for Direct2D's blur.

use std::cell::RefCell;

use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Graphics::Effects::{IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl};
use windows::UI::Composition::{CompositionBrush, CompositionEffectBrush, CompositionEffectSourceParameter, Compositor};
use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::Graphics::Direct2D::CLSID_D2D1GaussianBlur;
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT, IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl,
};
use windows::core::{Error, GUID, HSTRING, Interface, PCWSTR, Result, implement};

#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct GaussianBlur {
    name: RefCell<HSTRING>,
    sigma: f32,
    source: IGraphicsEffectSource,
}

impl IGraphicsEffect_Impl for GaussianBlur_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self.name.borrow().clone())
    }

    fn SetName(&self, name: &HSTRING) -> Result<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectSource_Impl for GaussianBlur_Impl {}

impl IGraphicsEffectD2D1Interop_Impl for GaussianBlur_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(CLSID_D2D1GaussianBlur)
    }

    fn GetNamedPropertyMapping(&self, name: &PCWSTR, index: *mut u32, mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING) -> Result<()> {
        let name = unsafe { name.to_string() }.unwrap_or_default();
        if name != "BlurAmount" {
            return Err(Error::from(E_INVALIDARG));
        }
        unsafe {
            *index = 0;
            *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
        }
        Ok(())
    }

    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(3)
    }

    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        let v = match index {
            0 => PropertyValue::CreateSingle(self.sigma)?, // standard deviation, in DIPs
            1 => PropertyValue::CreateUInt32(1)?,          // optimization: balanced
            2 => PropertyValue::CreateUInt32(1)?,          // border: hard (no dark rim at the edges)
            _ => return Err(Error::from(E_INVALIDARG)),
        };
        v.cast()
    }

    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        if index == 0 { Ok(self.source.clone()) } else { Err(Error::from(E_INVALIDARG)) }
    }

    fn GetSourceCount(&self) -> Result<u32> {
        Ok(1)
    }
}

/// A brush that shows `source` blurred by `sigma` (pixels).
pub fn brush(c: &Compositor, source: &CompositionBrush, sigma: f32) -> Result<CompositionEffectBrush> {
    let input: IGraphicsEffectSource = CompositionEffectSourceParameter::Create(&HSTRING::from("source"))?.cast()?;
    let effect: IGraphicsEffect = GaussianBlur { name: RefCell::new(HSTRING::from("blur")), sigma, source: input }.into();
    let factory = c.CreateEffectFactory(&effect)?;
    let brush = factory.CreateBrush()?;
    brush.SetSourceParameter(&HSTRING::from("source"), source)?;
    Ok(brush)
}
